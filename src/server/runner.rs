use super::{
    Server,
    events::{Job, Kind},
    reconcile,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::{Mutex, watch},
};

/// The executable and optional prefix are injectable without changing production argv.
pub struct Runner {
    pub executable: PathBuf,
    pub prefix: Vec<String>,
}

impl Runner {
    pub fn current() -> Result<Self> {
        Ok(Self {
            executable: std::env::current_exe()?,
            prefix: vec![],
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(&self.prefix);
        command
    }
}

pub fn child_env(token_env: &str, token: &str, data: &Path, repo: &str) -> Vec<(String, String)> {
    let encoded = BASE64_STANDARD.encode(format!("x-access-token:{token}"));
    crate::redact::register(&encoded);
    let header = format!("AUTHORIZATION: basic {encoded}");
    crate::redact::register(&header);
    vec![
        (token_env.into(), token.into()),
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        (
            "GIT_CONFIG_KEY_0".into(),
            "http.https://github.com/.extraheader".into(),
        ),
        ("GIT_CONFIG_VALUE_0".into(), header),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        (
            "XDG_CACHE_HOME".into(),
            data.join("cache").join(repo).display().to_string(),
        ),
    ]
}

pub fn child_env_with_secrets(
    token_env: &str,
    token: &str,
    data: &Path,
    repo: &str,
    secrets: &std::collections::BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let mut env: Vec<_> = secrets
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.extend(child_env(token_env, token, data, repo));
    env
}

struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

fn isolated(command: &mut Command) {
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
}

type Log = Arc<Mutex<std::fs::File>>;

async fn pump(input: impl AsyncRead + Unpin, log: Log) -> Result<()> {
    use std::io::Write;
    let mut reader = BufReader::new(input);
    let mut line = String::new();
    let mut pem = false;
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            pem = true;
        }
        let text = if pem {
            "[REDACTED]\n".into()
        } else {
            crate::redact::text(&line)
        };
        log.lock().await.write_all(text.as_bytes())?;
        if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
            pem = false;
        }
    }
    Ok(())
}

async fn logged(command: &mut Command, log: Log) -> Result<i32> {
    isolated(command);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let _group = ProcessGroup(child.id());
    let stdout = pump(
        child.stdout.take().context("missing child stdout")?,
        log.clone(),
    );
    let stderr = pump(child.stderr.take().context("missing child stderr")?, log);
    let (status, (), ()) = tokio::try_join!(
        async { Ok::<_, anyhow::Error>(child.wait().await?) },
        stdout,
        stderr
    )?;
    Ok(status.code().unwrap_or(1))
}

async fn git(ws: &Path, env: &[(String, String)], args: &[&str], log: Log) -> Result<()> {
    let mut command = Command::new("git");
    command.current_dir(ws).envs(env.iter().cloned()).args(args);
    if logged(&mut command, log).await? != 0 {
        bail!("git command failed (see job log)");
    }
    Ok(())
}

pub(super) async fn config(
    server: &Server,
    ws: &Path,
    base: &str,
    env: &[(String, String)],
) -> Result<(PathBuf, crate::config::Config)> {
    let blob = crate::git::read_blob_with_env(ws, base, "revera.yaml", 1024 * 1024, env).await?;
    match blob {
        Some(bytes) => Ok((
            PathBuf::from("revera.yaml"),
            server
                .dashboard
                .lock()
                .await
                .parse(std::str::from_utf8(&bytes)?)?,
        )),
        None => Ok((
            server.config.clone(),
            server
                .dashboard
                .lock()
                .await
                .parse(&std::fs::read_to_string(&server.config)?)?,
        )),
    }
}

pub fn strip_state(summary: &str) -> String {
    let mut output = String::new();
    let mut rest = summary;
    while let Some(start) = rest.find("<!-- revera-state:") {
        output.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("-->") else {
            rest = "";
            break;
        };
        rest = &rest[start + end + 3..];
    }
    output.push_str(rest);
    crate::text::excerpt_bytes(&crate::redact::text(&output), 65535)
}

pub fn conclusion(
    exit: i32,
    report: Option<&Value>,
    cancelled: bool,
) -> (&'static str, String, String) {
    if cancelled {
        return (
            "cancelled",
            "Review cancelled".into(),
            "Superseded or closed.".into(),
        );
    }
    let status = report.and_then(|r| r["status"].as_str());
    let findings = report
        .map(|report| accepted_findings(report).count())
        .unwrap_or(0);
    let result = if exit == 2 {
        "neutral"
    } else if exit != 0 || status != Some("complete") {
        "failure"
    } else if findings == 0 {
        "success"
    } else {
        "neutral"
    };
    let title = if exit == 2 {
        "Review partial".into()
    } else if result == "failure" {
        "Review failed".into()
    } else if findings > 0 {
        format!("{findings} findings")
    } else {
        "No findings".into()
    };
    let summary = report
        .and_then(|r| r["plan"]["summary_markdown"].as_str())
        .map(strip_state)
        .unwrap_or_else(|| "Review ended without a report. See the server job log.".into());
    (result, title, summary)
}

struct Execution {
    token: String,
    check: Option<u64>,
    report: PathBuf,
}

pub fn accepted_findings(report: &Value) -> impl Iterator<Item = &Value> {
    report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|finding| finding["validation_status"].as_str() == Some("accepted"))
}

pub const CLEAN_ARGS: &[&str] = &[
    "clean",
    "-ffdx",
    "-e",
    ".vera",
    "-e",
    ".revera/vera-cache.json",
];

async fn execute(
    server: &Server,
    job: &Job,
    app: &super::app::App,
    execution: &mut Execution,
    log: Log,
) -> Result<i32> {
    let relative = PathBuf::from("repos").join(&job.repo.full_name);
    let ws = crate::fsutil::ensure_dir(&server.settings.data_dir, &relative)?;
    if matches!(job.kind, Kind::Remove) {
        std::fs::remove_dir_all(&ws)?;
        return Ok(0);
    }
    execution.token = server
        .api
        .installation_token(
            app,
            job.repo.installation,
            Some(job.repo.id),
            if matches!(job.kind, Kind::Review { .. }) {
                super::app::Permissions::Review
            } else {
                super::app::Permissions::Index
            },
        )
        .await?;
    crate::fsutil::ensure_dir(
        &server.settings.data_dir,
        &Path::new("cache").join(&job.repo.full_name),
    )?;
    let env = child_env(
        "GITHUB_TOKEN",
        &execution.token,
        &server.settings.data_dir,
        &job.repo.full_name,
    );
    if std::fs::symlink_metadata(ws.join(".git")).is_ok() {
        crate::fsutil::ensure_dir(&ws, Path::new(".git"))?;
    } else {
        git(
            &ws,
            &env,
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                &format!("https://github.com/{}.git", job.repo.full_name),
                ".",
            ],
            log.clone(),
        )
        .await?;
        crate::fsutil::ensure_dir(&ws, Path::new(".git"))?;
    }
    let (head, base) = match &job.kind {
        Kind::Review { number, pr, .. } => {
            let head = pr["head"]["sha"].as_str().context("PR missing head sha")?;
            let base = pr["base"]["sha"].as_str().context("PR missing base sha")?;
            if !crate::git::is_oid(head) || !crate::git::is_oid(base) {
                bail!("PR has invalid commit ids");
            }
            git(
                &ws,
                &env,
                &["fetch", "origin", &format!("refs/pull/{number}/head")],
                log.clone(),
            )
            .await?;
            git(&ws, &env, &["fetch", "origin", head, base], log.clone()).await?;
            (head.to_string(), base.to_string())
        }
        Kind::Index => {
            git(
                &ws,
                &env,
                &[
                    "fetch",
                    "origin",
                    &format!("refs/heads/{}", job.repo.default_branch),
                ],
                log.clone(),
            )
            .await?;
            let head = crate::git::rev_parse_with_env(&ws, "FETCH_HEAD", &env).await?;
            (head.clone(), head)
        }
        Kind::Remove => unreachable!(),
    };
    git(
        &ws,
        &env,
        &["checkout", "--force", "--detach", &head],
        log.clone(),
    )
    .await?;
    git(&ws, &env, CLEAN_ARGS, log.clone()).await?;
    let (config_path, cfg) = config(server, &ws, &base, &env).await?;
    cfg.check_trust(true)?;
    // Register model credentials for the server-side log redactor, only after trust.
    let routes = [&cfg.models.investigator, cfg.effective_validator()]
        .into_iter()
        .chain(cfg.models.lead.iter())
        .chain(cfg.models.workers.iter().flatten())
        .chain(cfg.models.scouts.iter().flatten().map(|scout| &scout.route));
    for route in routes {
        if let Some(name) = &route.api_key_env {
            let _ = crate::redact::secret_env(name);
        }
    }
    if let Some(e) = &cfg.vera.embedding {
        let _ = crate::redact::secret_env(&e.api_key_env);
    }
    if let Some(e) = &cfg.vera.reranker {
        let _ = crate::redact::secret_env(&e.api_key_env);
    }
    let mut command = server.runner.command();
    let secrets = server.dashboard.lock().await.secrets.clone();
    command.current_dir(&ws).envs(child_env_with_secrets(
        &cfg.github.token_env,
        &execution.token,
        &server.settings.data_dir,
        &job.repo.full_name,
        &secrets,
    ));
    match &job.kind {
        Kind::Review { number, force, .. } => {
            let body = json!({
                "name": "Revera",
                "head_sha": head,
                "status": "in_progress",
                "external_id": number.to_string()
            });
            let response = server
                .api
                .request(
                    reqwest::Method::POST,
                    &format!("/repos/{}/check-runs", job.repo.full_name),
                    &execution.token,
                    Some(body),
                )
                .await?;
            execution.check = Some(
                response["id"]
                    .as_u64()
                    .context("check response missing id")?,
            );
            let mut summary = server
                .dashboard
                .lock()
                .await
                .jobs
                .get(&job.id)
                .cloned()
                .context("job summary missing")?;
            summary.check_run_url = response["html_url"].as_str().map(str::to_owned);
            summary.check = Some(super::dashboard::CheckRecovery {
                id: execution.check.unwrap(),
                repo: job.repo.clone(),
                pending: true,
            });
            server.update_job(summary).await?;
            let event = execution.report.parent().unwrap().join("event.json");
            let mut value = job.event();
            crate::redact::json(&mut value);
            crate::fsutil::write_output(
                &server.settings.data_dir,
                &event,
                &serde_json::to_vec(&value)?,
            )?;
            command
                .arg("review")
                .arg("--repo")
                .arg(&ws)
                .arg("--event")
                .arg(event)
                .arg("--publish")
                .arg("comment")
                .arg("--out")
                .arg(&execution.report)
                .arg("--bot-login")
                .arg(format!("{}[bot]", app.slug));
            if *force {
                command.arg("--force");
            }
        }
        Kind::Index => {
            command.arg("index").arg("--repo").arg(&ws);
        }
        Kind::Remove => unreachable!(),
    }
    command.arg("--config").arg(config_path);
    logged(&mut command, log).await
}

pub async fn run(server: Arc<Server>, job: &Job, cancel: watch::Receiver<bool>) {
    let result = run_inner(&server, job, cancel).await;
    if let Err(e) = result {
        let reason = crate::redact::diagnostic(&format!("{e:#}"));
        let _ = server.transition(job, "failed", Some(reason.clone())).await;
        tracing::warn!(
            "job failed: {}",
            crate::redact::diagnostic(&format!("{e:#}"))
        );
    }
}

fn prepare_job(server: &Server, job: &Job) -> Result<(PathBuf, Log, Execution)> {
    let dir = super::dashboard::job_dir(&server.settings.data_dir, &job.id)?;
    let file = crate::fsutil::create_private(&dir, "log.txt")?;
    let execution = Execution {
        token: String::new(),
        check: None,
        report: dir.join("report.json"),
    };
    Ok((dir, Arc::new(Mutex::new(file)), execution))
}

async fn cancellation(mut cancel: watch::Receiver<bool>) {
    if !*cancel.borrow() {
        let _ = cancel.changed().await;
    }
}

async fn bounded_execute(
    server: &Server,
    job: &Job,
    app: &super::app::App,
    execution: &mut Execution,
    log: Log,
) -> Result<i32> {
    let work = execute(server, job, app, execution, log.clone());
    match tokio::time::timeout(Duration::from_secs(55 * 60), work).await {
        Ok(Ok(code)) => Ok(code),
        failure => {
            let message = match failure {
                Ok(Err(e)) => format!("{e:#}"),
                Err(_) => "job exceeded 55 minute limit".into(),
                _ => unreachable!(),
            };
            use std::io::Write;
            writeln!(
                log.lock().await,
                "error: {}",
                crate::redact::diagnostic(&message)
            )?;
            Ok(1)
        }
    }
}

async fn complete_check(
    server: &Server,
    job: &Job,
    execution: &Execution,
    exit: i32,
    cancelled: bool,
) -> Result<()> {
    let Some(check) = execution.check else {
        return Ok(());
    };
    let report = std::fs::read(&execution.report)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let (conclusion, title, summary) = conclusion(exit, report.as_ref(), cancelled);
    let body = json!({
        "status": "completed",
        "conclusion": conclusion,
        "completed_at": chrono::Utc::now().to_rfc3339(),
        "output": {"title": title, "summary": summary}
    });
    let mut summary = server
        .dashboard
        .lock()
        .await
        .jobs
        .get(&job.id)
        .cloned()
        .context("job summary missing")?;
    if let Some(check) = &mut summary.check {
        check.pending = true;
    }
    server.update_job(summary).await?;
    server
        .api
        .request(
            reqwest::Method::PATCH,
            &format!("/repos/{}/check-runs/{check}", job.repo.full_name),
            &execution.token,
            Some(body),
        )
        .await?;
    let mut summary = server
        .dashboard
        .lock()
        .await
        .jobs
        .get(&job.id)
        .cloned()
        .context("job summary missing")?;
    if let Some(check) = &mut summary.check {
        check.pending = false;
    }
    server.update_job(summary).await?;
    Ok(())
}

async fn record_finish(server: &Server, job: &Job, cancelled: bool) -> Result<()> {
    if !cancelled {
        let mut seen = server.seen.lock().await;
        seen.finished(job);
        reconcile::save(server, &seen).await?;
    }
    Ok(())
}

async fn run_inner(server: &Server, job: &Job, cancel: watch::Receiver<bool>) -> Result<()> {
    if matches!(job.kind, Kind::Remove) {
        let path = Path::new("repos").join(&job.repo.full_name);
        let ws = crate::fsutil::ensure_dir(&server.settings.data_dir, &path)?;
        std::fs::remove_dir_all(ws)?;
        server
            .dashboard
            .lock()
            .await
            .repos
            .remove(&job.repo.full_name);
        let _ = server.events.send(("repos", json!({})));
        return record_finish(server, job, false).await;
    }
    let app = server
        .setup
        .lock()
        .await
        .app
        .clone()
        .context("app not configured")?;
    let (dir, log, mut execution) = prepare_job(server, job)?;
    server.transition(job, "running", None).await?;
    let work = bounded_execute(server, job, &app, &mut execution, log);
    let (exit, cancelled) = tokio::select! {
        biased;
        _ = cancellation(cancel.clone()) => (1, true),
        result = work => (result?, false),
    };
    let mut cancelled = cancelled || *cancel.borrow();
    loop {
        if let Err(e) = complete_check(server, job, &execution, exit, cancelled).await {
            tracing::warn!(
                "check completion failed: {}",
                crate::redact::diagnostic(&format!("{e:#}"))
            );
        }
        // All queue cancellation sends use this lock: the final commit cannot race them.
        let mut queue = server.queue.lock().await;
        if !cancelled && *cancel.borrow() {
            cancelled = true;
            drop(queue);
            continue;
        }
        record_finish(server, job, cancelled).await?;
        finish_summary(server, job, &execution, exit, cancelled).await?;
        if queue
            .running
            .get(&job.key())
            .is_some_and(|running| running.job.id == job.id)
        {
            queue.running.remove(&job.key());
        }
        break;
    }
    tracing::info!("job finished: exit={exit}, cancelled={cancelled}");
    let event = dir.join("event.json");
    crate::fsutil::check_regular_target(&event)?;
    if event.exists() {
        std::fs::remove_file(event)?;
    }
    server.prune_jobs().await?;
    Ok(())
}

async fn finish_summary(
    server: &Server,
    job: &Job,
    execution: &Execution,
    exit: i32,
    cancelled: bool,
) -> Result<()> {
    let report = std::fs::read(&execution.report)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let status = if cancelled {
        "cancelled"
    } else if exit == 2 {
        "partial"
    } else if exit == 0 {
        "complete"
    } else {
        "failed"
    };
    let reason = report
        .as_ref()
        .and_then(|r| r["reason"].as_str())
        .map(str::to_owned);
    let skipped = !cancelled && reason.as_ref().is_some_and(|s| s.starts_with("fork PR:"));
    let mut summary = server
        .dashboard
        .lock()
        .await
        .jobs
        .get(&job.id)
        .cloned()
        .context("job summary missing")?;
    summary.set_state(if skipped { "skipped" } else { status }, reason);
    if let Some(report) = report {
        summary.findings = super::dashboard::FindingCounts::default();
        for finding in accepted_findings(&report) {
            summary.findings.total += 1;
            match finding["severity"].as_str() {
                Some("high") => summary.findings.high += 1,
                Some("medium") => summary.findings.medium += 1,
                Some("low") => summary.findings.low += 1,
                _ => {}
            }
        }
        if !report["ledger"].is_null() {
            summary.tokens = Some(json!({
                "prompt": report["ledger"]["prompt_tokens"].as_u64().unwrap_or(0),
                "completion": report["ledger"]["completion_tokens"].as_u64().unwrap_or(0),
                "cached": report["ledger"]["cached_prompt_tokens"].as_u64().unwrap_or(0)
            }));
        }
    }
    server.update_job(summary).await
}
