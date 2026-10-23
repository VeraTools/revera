use super::{
    Server, dashboard,
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
        ("GIT_CONFIG_COUNT".into(), "3".into()),
        (
            "GIT_CONFIG_KEY_0".into(),
            "http.https://github.com/.extraheader".into(),
        ),
        ("GIT_CONFIG_VALUE_0".into(), header),
        // auto-maintenance after fetch daemonizes by default; the detached
        // process outlives the job and is left unreaped by the server
        ("GIT_CONFIG_KEY_1".into(), "gc.autoDetach".into()),
        ("GIT_CONFIG_VALUE_1".into(), "false".into()),
        ("GIT_CONFIG_KEY_2".into(), "maintenance.autoDetach".into()),
        ("GIT_CONFIG_VALUE_2".into(), "false".into()),
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

pub const UNCONFIGURED: &str = "models are not configured yet";

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
    indexed_sha: Option<String>,
}

pub fn accepted_findings(report: &Value) -> impl Iterator<Item = &Value> {
    crate::findings::accepted_values(report)
}

pub const CLEAN_ARGS: &[&str] = &[
    "clean",
    "-ffdx",
    "-e",
    ".vera",
    "-e",
    ".revera/vera-cache.json",
];

/// Clear ambient credentials and git configuration for public targets. Only
/// operator-configured model/search credentials are deliberately added below.
fn anonymous_env(data: &Path, repo_id: u64) -> Vec<(String, String)> {
    let root = data.join("external").join(repo_id.to_string());
    let mut env = vec![
        (
            "PATH".into(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
        ),
        ("HOME".into(), root.join("home").display().to_string()),
        (
            "XDG_CONFIG_HOME".into(),
            root.join("home/config").display().to_string(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            root.join("cache").display().to_string(),
        ),
        (
            "REVERA_VERA_HOME".into(),
            root.join("cache/vera-home").display().to_string(),
        ),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
    ];
    let settings = [
        ("core.hooksPath", "/dev/null"),
        ("core.fsmonitor", "false"),
        ("credential.helper", ""),
        ("http.extraHeader", ""),
        ("http.followRedirects", "false"),
        ("protocol.allow", "never"),
        ("protocol.https.allow", "always"),
        ("gc.autoDetach", "false"),
        ("maintenance.autoDetach", "false"),
    ];
    env.push(("GIT_CONFIG_COUNT".into(), settings.len().to_string()));
    for (i, (key, value)) in settings.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), key.into()));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), value.into()));
    }
    env
}

async fn anonymous_git(ws: &Path, env: &[(String, String)], args: &[&str], log: Log) -> Result<()> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .envs(env.iter().cloned())
        .current_dir(ws)
        .args(args)
        .stdin(Stdio::null());
    if logged(&mut command, log).await? != 0 {
        bail!("anonymous git command failed (see job log)");
    }
    Ok(())
}

async fn prepare_external(
    ws: &Path,
    remote: &str,
    number: u64,
    head: &str,
    base: &str,
    env: &[(String, String)],
    log: Log,
) -> Result<()> {
    if !crate::git::is_oid(head) || !crate::git::is_oid(base) {
        bail!("PR has invalid commit ids");
    }
    if std::fs::symlink_metadata(ws.join(".git")).is_ok() {
        crate::fsutil::ensure_dir(ws, Path::new(".git"))?;
        clear_checkout_locks(ws)?;
        anonymous_git(
            ws,
            env,
            &["remote", "set-url", "origin", remote],
            log.clone(),
        )
        .await?;
    } else {
        anonymous_git(ws, env, &["init", "--template=", "."], log.clone()).await?;
        anonymous_git(ws, env, &["remote", "add", "origin", remote], log.clone()).await?;
    }
    anonymous_git(
        ws,
        env,
        &["config", "remote.origin.promisor", "true"],
        log.clone(),
    )
    .await?;
    anonymous_git(
        ws,
        env,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
        log.clone(),
    )
    .await?;
    // Fork heads live in the base repository's read-only pull namespace. Fetch
    // the recorded SHA too: a force-push between resolution and fetch must never
    // make us review FETCH_HEAD instead of the pinned revision.
    anonymous_git(
        ws,
        env,
        &[
            "fetch",
            "--no-tags",
            "--filter=blob:none",
            "origin",
            &format!("refs/pull/{number}/head"),
            head,
            base,
        ],
        log.clone(),
    )
    .await?;
    anonymous_git(
        ws,
        env,
        &["checkout", "--force", "--detach", head],
        log.clone(),
    )
    .await?;
    anonymous_git(ws, env, CLEAN_ARGS, log).await
}

async fn execute_external(
    server: &Server,
    job: &Job,
    execution: &mut Execution,
    log: Log,
) -> Result<i32> {
    if job.repo.installation.is_some() {
        bail!("external execution cannot use an installation");
    }
    let Kind::Review { number, pr, force } = &job.kind else {
        bail!("external targets support reviews only");
    };
    let yaml = server.config_yaml()?.context(UNCONFIGURED)?;
    let (cfg, env) = {
        let dashboard = server.dashboard.lock().await;
        let cfg = dashboard.parse(&yaml)?;
        cfg.check_trust(true)?;
        let mut env = anonymous_env(&server.settings.data_dir, job.repo.id);
        let routes = [&cfg.models.investigator, cfg.effective_validator()]
            .into_iter()
            .chain(cfg.models.lead.iter())
            .chain(cfg.models.workers.iter().flatten())
            .chain(cfg.models.scouts.iter().flatten().map(|scout| &scout.route));
        let names = dashboard::Dashboard::referenced(&yaml)
            .into_iter()
            .chain(
                routes
                    .flat_map(|route| route.routes())
                    .filter_map(|route| route.api_key_env.clone()),
            )
            .chain(
                cfg.vera
                    .embedding
                    .iter()
                    .chain(cfg.vera.reranker.iter())
                    .map(|e| e.api_key_env.clone()),
            )
            .chain(std::iter::once(cfg.web_search.api_key_env.clone()));
        for name in names {
            // Never allow reserved names, even if the operator referenced them.
            if crate::config::check_credential_env(
                "external credential",
                &name,
                &cfg.github.token_env,
            )
            .is_ok()
                && let Some(value) = dashboard.env(&name)
            {
                env.push((name, value));
            }
        }
        (cfg, env)
    };
    if cfg.vera.executable.contains('/') && !Path::new(&cfg.vera.executable).is_absolute() {
        bail!("external reviews require a trusted absolute Vera executable or PATH name");
    }
    let relative = PathBuf::from("external").join(job.repo.id.to_string());
    for dir in ["home", "home/config", "cache", "cache/vera-home"] {
        crate::fsutil::ensure_dir(&server.settings.data_dir, &relative.join(dir))?;
    }
    // Per-PR workspaces isolate local review history as well as installed jobs.
    let ws = crate::fsutil::ensure_dir(
        &server.settings.data_dir,
        &relative.join(format!("pull-{number}")),
    )?;
    prepare_external(
        &ws,
        &format!("https://github.com/{}.git", job.repo.full_name),
        *number,
        pr["head"]["sha"].as_str().context("missing head")?,
        pr["base"]["sha"].as_str().context("missing base")?,
        &env,
        log.clone(),
    )
    .await?;
    let event = execution.report.parent().unwrap().join("event.json");
    let mut value = job.event();
    value["repository"]["private"] = json!(false);
    crate::redact::json(&mut value);
    crate::fsutil::write_output(
        &server.settings.data_dir,
        &event,
        &serde_json::to_vec(&value)?,
    )?;
    let mut command = server.runner.command();
    command
        .env_clear()
        .envs(env)
        .current_dir(&ws)
        .stdin(Stdio::null())
        .arg("review")
        .arg("--repo")
        .arg(&ws)
        .arg("--event")
        .arg(event)
        .arg("--publish")
        .arg("dry-run")
        .arg("--private")
        .arg("--review-url")
        .arg(format!(
            "{}/reviews/{}",
            server.settings.public_url.trim_end_matches('/'),
            job.id
        ))
        .arg("--config")
        .arg(&server.config)
        .arg("--out")
        .arg(&execution.report);
    if *force {
        command.arg("--force");
    }
    if let Some(profile) = job.mode.profile() {
        command.arg("--profile").arg(profile);
    }
    logged(&mut command, log).await
}

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
            job.repo.installation_id()?,
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
        clear_checkout_locks(&ws)?;
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
            execution.indexed_sha = Some(head.clone());
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
                .arg(format!("{}[bot]", app.slug))
                .arg("--review-url")
                .arg(format!(
                    "{}/reviews/{}",
                    server.settings.public_url.trim_end_matches('/'),
                    job.id
                ));
            if server.dashboard.lock().await.preferences.fixes.offer {
                command.arg("--fix-url").arg(format!(
                    "{}/fixes/new?job={}",
                    server.settings.public_url, job.id
                ));
            }
            if *force {
                command.arg("--force");
            }
            if let Some(profile) = job.mode.profile() {
                command.arg("--profile").arg(profile);
            }
            let preference = server
                .dashboard
                .lock()
                .await
                .preferences
                .repos
                .get(&job.repo.full_name)
                .cloned()
                .unwrap_or_default();
            if let Some(guidance) = preference.guidance {
                command.arg("--guidance").arg(guidance.as_str());
            }
            if let Some(notes) = preference.notes {
                let path = execution.report.parent().unwrap().join("notes.md");
                crate::fsutil::write_output(
                    &server.settings.data_dir,
                    &path,
                    crate::redact::text(&notes).as_bytes(),
                )?;
                command.arg("--notes-file").arg(path);
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
    if matches!(job.kind, Kind::Review { .. })
        && let Err(error) = super::fixes::review_finished(&server, job).await
    {
        tracing::warn!(
            "fix review completion: {}",
            crate::redact::diagnostic(&error.to_string())
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
        indexed_sha: None,
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
    let work = async {
        if job.repo.source() == super::events::Source::External {
            execute_external(server, job, execution, log.clone()).await
        } else {
            execute(server, job, app, execution, log.clone()).await
        }
    };
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
    let mut body = json!({
        "status": "completed",
        "conclusion": conclusion,
        "completed_at": chrono::Utc::now().to_rfc3339(),
        "output": {"title": title, "summary": summary}
    });
    if !cancelled
        && exit == 0
        && report.as_ref().is_some_and(|r| {
            r["status"] == "complete"
                && (accepted_findings(r).next().is_some()
                    || !super::api::report_carried(r).is_empty())
        })
        && server.dashboard.lock().await.preferences.fixes.offer
    {
        body["actions"] = json!([{"label":"Fix with Revera","description":"Open a fix request","identifier":"revera_fix"}]);
    }
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

async fn record_finish(
    server: &Server,
    job: &Job,
    cancelled: bool,
    indexed_sha: Option<&str>,
) -> Result<()> {
    if !cancelled {
        let mut seen = server.seen.lock().await;
        seen.finished(job, indexed_sha);
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
        return record_finish(server, job, false, None).await;
    }
    if server.config_yaml()?.is_none() {
        server
            .transition(job, "skipped", Some(UNCONFIGURED.into()))
            .await?;
        return server.prune_jobs().await;
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
        if exit == 0 || !matches!(job.kind, Kind::Index) {
            // With retrieval disabled `revera index` succeeds without building
            // an index; only a ready index counts as the commit being indexed.
            let ready = server
                .settings
                .data_dir
                .join("repos")
                .join(&job.repo.full_name)
                .join(".vera/vectors.manifest")
                .is_file();
            let indexed = execution.indexed_sha.as_deref().filter(|_| ready);
            record_finish(server, job, cancelled, indexed).await?;
        } else if !cancelled {
            // A failed run may have discarded the previous index (for example
            // after an identity change), so no commit is known to be indexed.
            let mut seen = server.seen.lock().await;
            seen.index_failed(job);
            reconcile::save(server, &seen).await?;
        }
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

fn clear_checkout_locks(ws: &Path) -> Result<()> {
    let git = crate::fsutil::ensure_dir(ws, Path::new(".git"))?;
    // The queue serializes checkouts. The fix controller shares this Git dir,
    // but its rev-parse/cat-file/fetch never take index.lock or HEAD.lock.
    // Other locks may belong to that controller and must be left alone.
    for name in ["index.lock", "HEAD.lock"] {
        let path = git.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                crate::fsutil::check_regular_target(&path)?;
                std::fs::remove_file(&path)?;
                tracing::info!(
                    "removed stale Git lock: {}",
                    crate::redact::text(&path.display().to_string())
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
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
    let reason = if status == "failed" {
        Some(failure_reason(
            &execution.report.with_file_name("log.txt"),
            exit,
        ))
    } else {
        reason
    };
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
        let counts = crate::findings::accepted_counts_json(&report);
        summary.findings = super::dashboard::FindingCounts {
            total: counts.total,
            high: counts.high,
            medium: counts.medium,
            low: counts.low,
        };
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

fn failure_reason(path: &Path, exit: i32) -> String {
    use std::io::BufRead;
    let mut last = None;
    if let Ok(file) = std::fs::File::open(path) {
        for line in std::io::BufReader::new(file).lines() {
            let Ok(line) = line else {
                break;
            };
            let line = line.trim();
            if line.starts_with("Error:") || line.starts_with("error:") {
                last = Some(crate::redact::diagnostic(line).chars().take(300).collect());
            }
        }
    }
    last.unwrap_or_else(|| format!("process exited with status {exit}"))
}

#[cfg(test)]
mod external_tests {
    use super::*;

    fn local_git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    #[tokio::test]
    async fn anonymous_fetch_pins_exact_shas_even_after_fork_ref_force_push() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        std::fs::create_dir(&source).unwrap();
        local_git(&source, &["init", "-q"]);
        std::fs::write(source.join("file.txt"), "base\n").unwrap();
        local_git(&source, &["add", "."]);
        local_git(&source, &["commit", "-qm", "base"]);
        let base = local_git(&source, &["rev-parse", "HEAD"]);
        std::fs::write(source.join("file.txt"), "fork head\n").unwrap();
        local_git(&source, &["commit", "-qam", "fork head"]);
        let head = local_git(&source, &["rev-parse", "HEAD"]);
        local_git(&source, &["update-ref", "refs/pull/7/head", &head]);
        std::fs::write(source.join("file.txt"), "force-pushed head\n").unwrap();
        local_git(&source, &["commit", "-qam", "force push"]);
        let newer = local_git(&source, &["rev-parse", "HEAD"]);
        local_git(&source, &["update-ref", "refs/pull/7/head", &newer]);
        let bare = scratch.path().join("remote.git");
        local_git(
            scratch.path(),
            &[
                "clone",
                "--bare",
                source.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        local_git(&bare, &["update-ref", "refs/pull/7/head", &newer]);
        let ws = crate::fsutil::ensure_dir(scratch.path(), Path::new("checkout")).unwrap();
        let mut env = anonymous_env(scratch.path(), 701);
        // Test transport only; production environment permits HTTPS and nothing else.
        let count: usize = env
            .iter()
            .find(|(k, _)| k == "GIT_CONFIG_COUNT")
            .map(|(_, v)| v.parse().unwrap())
            .unwrap();
        env.retain(|(k, _)| k != "GIT_CONFIG_COUNT");
        env.push(("GIT_CONFIG_COUNT".into(), (count + 1).to_string()));
        env.push((
            format!("GIT_CONFIG_KEY_{count}"),
            "protocol.file.allow".into(),
        ));
        env.push((format!("GIT_CONFIG_VALUE_{count}"), "always".into()));
        let log = Arc::new(Mutex::new(
            crate::fsutil::create_private(scratch.path(), "git.log").unwrap(),
        ));
        prepare_external(
            &ws,
            &format!("file://{}", bare.display()),
            7,
            &head,
            &base,
            &env,
            log.clone(),
        )
        .await
        .unwrap();
        assert_eq!(local_git(&ws, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            std::fs::read_to_string(ws.join("file.txt")).unwrap(),
            "fork head\n"
        );
        assert_eq!(local_git(&ws, &["rev-parse", &base]), base);
        for name in ["index.lock", "HEAD.lock"] {
            std::fs::write(ws.join(".git").join(name), "").unwrap();
        }
        prepare_external(
            &ws,
            &format!("file://{}", bare.display()),
            7,
            &newer,
            &base,
            &env,
            log,
        )
        .await
        .unwrap();
        assert_eq!(local_git(&ws, &["rev-parse", "HEAD"]), newer);
        for name in ["index.lock", "HEAD.lock"] {
            assert!(!ws.join(".git").join(name).exists());
        }
        assert_eq!(
            std::fs::read_to_string(ws.join("file.txt")).unwrap(),
            "force-pushed head\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn checkout_lock_cleanup_refuses_symlinks_and_non_files() {
        let scratch = tempfile::tempdir().unwrap();
        let git = crate::fsutil::ensure_dir(scratch.path(), Path::new(".git")).unwrap();
        let victim = scratch.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let lock = git.join("index.lock");
        std::os::unix::fs::symlink(&victim, &lock).unwrap();
        assert!(clear_checkout_locks(scratch.path()).is_err());
        assert!(
            std::fs::symlink_metadata(&lock)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        assert!(clear_checkout_locks(scratch.path()).is_err());
        assert!(lock.is_dir());
        std::fs::remove_dir(&lock).unwrap();
        std::fs::remove_dir(&git).unwrap();
        let other = scratch.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&other, &git).unwrap();
        assert!(clear_checkout_locks(scratch.path()).is_err());
    }

    #[tokio::test]
    async fn external_child_clear_environment_has_no_github_or_ambient_credentials() {
        let scratch = tempfile::tempdir().unwrap();
        let runner = Runner {
            executable: "/bin/sh".into(),
            prefix: vec![],
        };
        let mut child = runner.command();
        child
            .env_clear()
            .envs(anonymous_env(scratch.path(), 701))
            .env("MODEL_KEY", "only-intended-model-credential")
            .args(["-c", "env"]);
        let result = child.output().await.unwrap();
        assert!(result.status.success());
        let environment = String::from_utf8(result.stdout).unwrap();
        assert!(environment.contains("MODEL_KEY=only-intended-model-credential"));
        for forbidden in [
            "GITHUB_TOKEN=",
            "GH_TOKEN=",
            "AUTHORIZATION",
            "REVERA_SETUP",
            "SSH_AUTH_SOCK=",
            "PRIVATE KEY",
            "GIT_ASKPASS=",
        ] {
            assert!(!environment.contains(forbidden), "{forbidden}");
        }
        assert!(environment.contains(&format!(
            "HOME={}/external/701/home",
            scratch.path().display()
        )));
    }
}
