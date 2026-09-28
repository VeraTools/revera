use crate::config::{Config, PublishMode, Strategy};
use crate::pipeline::common::ReviewRequest;
use crate::pipeline::run as pipeline_run;
use crate::report::{surfaced_ids, RunStatus};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "revera",
    about = "Provider-independent PR reviewer with Vera retrieval",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Review {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        head: Option<String>,
        /// GitHub event payload path (pull_request / pull_request_target).
        #[arg(long)]
        event: Option<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        strategy: Option<StrategyArg>,
        #[arg(long)]
        publish: Option<PublishArg>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body_file: Option<PathBuf>,
        /// Re-review even when the patch is identical to stored state.
        #[arg(long)]
        force: bool,
    },
    Doctor {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        strategy: Option<StrategyArg>,
        #[arg(long)]
        publish: Option<PublishArg>,
    },
    CacheInfo {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Print the Vera index cache identity (backend, embedding model,
    /// exclusions, ...) as a short hash. Investigator/validator settings
    /// do not affect it.
    CacheKey {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        /// PR event JSON: an in-checkout config is read from the event's
        /// base commit, exactly as `review --event` does.
        #[arg(long)]
        event: Option<PathBuf>,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum StrategyArg {
    Baseline,
    Delegated,
    Panel,
}

#[derive(Clone, Copy, ValueEnum)]
enum PublishArg {
    DryRun,
    Comment,
}

pub async fn run() -> i32 {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Doctor {
            config,
            profile,
            strategy,
            publish,
        } => doctor(config, profile, strategy, publish).await,
        Cmd::CacheInfo { repo } => cache_info(&repo),
        Cmd::CacheKey {
            config,
            profile,
            event,
            repo,
        } => cache_key(config, profile, event, repo).await,
        Cmd::Review {
            repo,
            base,
            head,
            event,
            config,
            profile,
            strategy,
            publish,
            out,
            title,
            body_file,
            force,
        } => {
            review(ReviewArgs {
                repo,
                base,
                head,
                event,
                config,
                profile,
                strategy,
                publish,
                out,
                title,
                body_file,
                force,
            })
            .await
        }
    }
}

struct ReviewArgs {
    repo: PathBuf,
    base: Option<String>,
    head: Option<String>,
    event: Option<PathBuf>,
    config: Option<PathBuf>,
    profile: Option<String>,
    strategy: Option<StrategyArg>,
    publish: Option<PublishArg>,
    out: Option<PathBuf>,
    title: Option<String>,
    body_file: Option<PathBuf>,
    force: bool,
}

fn load_cfg_unvalidated(
    path: Option<&std::path::Path>,
    profile: Option<&str>,
) -> Result<(PathBuf, Config), String> {
    let p = Config::find(path).map_err(|e| e.to_string())?;
    let mut c = Config::load(&p).map_err(|e| e.to_string())?;
    if let Some(pr) = profile {
        c.apply_profile(pr).map_err(|e| e.to_string())?;
    }
    Ok((p, c))
}

/// Lexically normalize `p` against `cwd` (no symlink resolution: a
/// symlink inside the checkout must not make PR content look external).
fn lexical_abs(cwd: &std::path::Path, p: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in cwd.join(p).components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Where the review configuration came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigOrigin {
    /// Local run: the working-tree file.
    WorkingTree(PathBuf),
    /// Event mode, config inside the checkout: read from the immutable
    /// base revision, never the PR head.
    Base { path: String, sha: String },
    /// Event mode, config outside the checkout (operator-provided).
    External(PathBuf),
}

impl std::fmt::Display for ConfigOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkingTree(p) => write!(f, "{}", p.display()),
            Self::Base { path, sha } => write!(f, "{path} at base {sha}"),
            Self::External(p) => write!(f, "{} (external)", p.display()),
        }
    }
}

/// Maximum size of a config file read from git.
const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// Event-mode config: a path inside the checkout is read from the base
/// commit (the PR cannot change the config that holds its credentials);
/// a path outside it is an operator-supplied trusted file.
pub async fn load_event_config(
    repo: &std::path::Path,
    config: Option<&std::path::Path>,
    profile: Option<&str>,
    base_sha: &str,
) -> Result<(ConfigOrigin, Config), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let wanted = config.unwrap_or(std::path::Path::new("revera.yaml"));
    let abs = lexical_abs(&cwd, wanted);
    let root = crate::git::repo_root(repo);
    // a path that only resolves into the checkout through a symlink is
    // still PR content
    let canon = abs.canonicalize().ok();
    let inside = abs
        .strip_prefix(&root)
        .ok()
        .or_else(|| canon.as_deref().and_then(|c| c.strip_prefix(&root).ok()));
    let (origin, mut cfg) = match inside {
        Some(rel) => {
            // the base commit holds the trusted config (shallow checkouts
            // may not have it yet)
            if !crate::git::has_commit(&root, base_sha).await {
                crate::git::fetch_sha(&root, base_sha)
                    .await
                    .map_err(|e| format!("cannot fetch base sha {base_sha}: {e:#}"))?;
            }
            let rel = rel
                .to_str()
                .ok_or("config path is not valid UTF-8")?
                .replace('\\', "/");
            let bytes = crate::git::read_blob(&root, base_sha, &rel, MAX_CONFIG_BYTES)
                .await
                .map_err(|e| format!("{e:#}"))?
                .ok_or_else(|| {
                    format!(
                        "config {rel} is not a regular file at base {base_sha}; in event mode the config is read from the base revision (merge it first, or pass an absolute --config outside the checkout)"
                    )
                })?;
            let text =
                String::from_utf8(bytes).map_err(|_| format!("config {rel} is not UTF-8"))?;
            let origin = ConfigOrigin::Base {
                path: rel,
                sha: base_sha.to_string(),
            };
            let cfg = Config::parse(&text, &origin.to_string()).map_err(|e| format!("{e:#}"))?;
            (origin, cfg)
        }
        None => (
            ConfigOrigin::External(abs.clone()),
            Config::load(&abs).map_err(|e| format!("{e:#}"))?,
        ),
    };
    if let Some(pr) = profile {
        cfg.apply_profile(pr).map_err(|e| e.to_string())?;
    }
    Ok((origin, cfg))
}

fn strategy_arg(s: StrategyArg) -> Strategy {
    match s {
        StrategyArg::Baseline => Strategy::Baseline,
        StrategyArg::Delegated => Strategy::Delegated,
        StrategyArg::Panel => Strategy::Panel,
    }
}

fn publish_arg(p: PublishArg) -> PublishMode {
    match p {
        PublishArg::DryRun => PublishMode::DryRun,
        PublishArg::Comment => PublishMode::Comment,
    }
}

/// Fork PR skipped before any model, Vera or credential access: write the
/// partial report and return exit 2.
fn fork_skip(
    a: &ReviewArgs,
    e: &crate::github::event::PrEvent,
    strategy: Strategy,
    reason: &str,
) -> i32 {
    let repo = crate::git::repo_root(&a.repo);
    let rep = crate::report::RunReport {
        status: crate::report::RunStatus::Partial,
        reason: Some(reason.into()),
        base: e.base_sha.clone(),
        head: e.head_sha.clone(),
        strategy: format!("{:?}", strategy).to_lowercase(),
        findings: vec![],
        plan: crate::report::PublicationPlan {
            inline: vec![],
            summary_markdown: crate::report::summary_markdown(&crate::report::Summary {
                coverage: reason,
                status: Some(RunStatus::Partial),
                reason: Some(reason),
                strategy: "none",
                ..Default::default()
            }),
            state: crate::state::ReviewState::default(),
            publish_uncertain: false,
        },
        ledger: crate::report::LedgerReport {
            requests: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            reasoning_tokens: 0,
            by_route: vec![],
            wall_ms: 0,
        },
        publication: crate::report::Publication {
            mode: "dry-run".into(),
            skipped_reason: Some(reason.into()),
            ..Default::default()
        },
        coverage_gaps: vec![],
        timing: Default::default(),
        stats: Default::default(),
    };
    print!("{}", rep.plan.summary_markdown);
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(".revera/last-report.json"));
    if let Err(e) = write_report(&repo, &out, &rep) {
        eprintln!("error: cannot write {}: {e:#}", out.display());
    }
    eprintln!("report: {}", out.display());
    2
}

async fn review(a: ReviewArgs) -> i32 {
    let strategy = a.strategy.map(strategy_arg);
    let repo = crate::git::repo_root(&a.repo);

    // ---- event mode ----
    let ev = match &a.event {
        Some(p) => match crate::github::event::parse(p) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("error: {e:#}");
                return 1;
            }
        },
        None => None,
    };
    if let Some(e) = &ev {
        if !crate::git::is_oid(&e.base_sha) || !crate::git::is_oid(&e.head_sha) {
            eprintln!("error: event base/head are not full commit ids");
            return 1;
        }
    }
    // credentials are not checked here: the fork guard must run (and emit
    // its partial report) before any credential check can abort the run
    let loaded = match &ev {
        Some(e) => {
            load_event_config(
                &repo,
                a.config.as_deref(),
                a.profile.as_deref(),
                &e.base_sha,
            )
            .await
        }
        None => load_cfg_unvalidated(a.config.as_deref(), a.profile.as_deref())
            .map(|(p, c)| (ConfigOrigin::WorkingTree(p), c)),
    };
    let (origin, cfg) = match loaded {
        Ok(c) => c,
        Err(err) => {
            // no trusted base config: a fork cannot be reviewed with
            // credentials anyway, so report the skip instead of failing
            if let Some(e) = ev.as_ref().filter(|e| e.is_fork()) {
                if a.publish.is_some_and(|p| matches!(p, PublishArg::Comment)) {
                    eprintln!("config: {err}");
                    let reason = "fork PR: review skipped (no trusted base config)";
                    eprintln!(
                        "revera: {reason} ({} -> {})",
                        e.head_repo_full_name, e.repo_full_name
                    );
                    return fork_skip(&a, e, strategy.unwrap_or_default(), reason);
                }
            }
            eprintln!("error: {err}");
            return 1;
        }
    };
    eprintln!("config: {origin}");
    let effective_strategy = strategy.unwrap_or(cfg.review.strategy);
    let publish = a.publish.map(publish_arg).unwrap_or(cfg.review.publish);
    if publish == PublishMode::Comment && a.event.is_none() {
        eprintln!("error: --publish comment requires --event");
        return 1;
    }
    if let Err(e) = cfg.check_publication(publish) {
        eprintln!("error: {e}");
        return 1;
    }

    // Fork guard: no model or Vera calls at all — return partial immediately.
    if let Some(e) = &ev {
        if e.is_fork() && publish == PublishMode::Comment && !cfg.github.allow_forks {
            let reason = "fork PR: review skipped (github.allow_forks=false)";
            eprintln!(
                "revera: {reason} ({} -> {})",
                e.head_repo_full_name, e.repo_full_name
            );
            return fork_skip(&a, e, effective_strategy, reason);
        }
    }
    // trust before credentials: no secret is resolved for an endpoint or
    // env name that fails these checks
    if let Err(e) = cfg
        .check_trust(ev.is_some())
        .and_then(|_| cfg.validate_for(effective_strategy))
    {
        eprintln!("error: {e}");
        return 1;
    }
    let body = a
        .body_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    // Event mode supplies base/head/title/body and GitHub-backed state.
    let api = ev.as_ref().map(|e| {
        let token = crate::redact::secret_env(&cfg.github.token_env).unwrap_or_default();
        (e.clone(), crate::github::api::GitHubApi::new(&token))
    });
    let (base, head, title, pr_body) = match &api {
        Some((e, api)) => {
            let current = match crate::git::current_head(&repo).await {
                Ok(current) => current,
                Err(err) => {
                    eprintln!("error: cannot determine checked-out HEAD: {err:#}");
                    return 1;
                }
            };
            if current != e.head_sha {
                if let Err(err) = crate::git::materialize_head(&repo, &e.head_sha).await {
                    if crate::git::tracked_dirty(&repo).await.unwrap_or(false) {
                        eprintln!("error: {err:#}");
                        return 2;
                    }
                    eprintln!("error: cannot check out PR head {}: {err:#}", e.head_sha);
                    return 1;
                }
                eprintln!(
                    "event: checked out PR head {} (was {})",
                    e.head_sha, current
                );
            }
            // seed state from our managed summary comment (fallback: empty)
            let (owner, rname) = e.owner_repo();
            let seeded = match api.list_issue_comments(owner, rname, e.number).await {
                Ok(comments) => {
                    let me =
                        crate::github::publish::Identity::resolve(api, &cfg.github.bot_login).await;
                    crate::github::publish::find_managed(
                        &comments,
                        &cfg.github.summary_marker,
                        None,
                        &me,
                    )
                    .and_then(|c| {
                        let mut s = crate::github::publish::decode_state(&c.body)?;
                        s.summary_comment_id = Some(c.id);
                        Some(s)
                    })
                    .unwrap_or_default()
                }
                Err(err) => {
                    tracing::warn!("could not list PR comments for state: {err:#}");
                    crate::state::ReviewState::default()
                }
            };
            let _ = seeded.save(&repo);
            (
                e.base_sha.clone(),
                Some(e.head_sha.clone()),
                Some(e.title.clone()),
                e.body.clone(),
            )
        }
        None => {
            let Some(b) = a.base.clone() else {
                eprintln!("error: --base is required without --event");
                return 1;
            };
            (b, a.head.clone(), a.title.clone(), body)
        }
    };

    let req = ReviewRequest {
        repo: a.repo.clone(),
        base,
        head,
        title,
        body: pr_body,
        strategy_override: strategy,
        force: a.force,
    };
    match pipeline_run(&cfg, &req).await {
        Ok((mut report, mut state)) => {
            let mut publish_failed = false;
            if let (Some((e, api)), PublishMode::Comment) = (&api, publish) {
                let publish_start = std::time::Instant::now();
                let publish_result = crate::github::publish::publish(
                    api,
                    e,
                    &mut report,
                    &mut state,
                    cfg.review.max_findings,
                    &cfg.github.summary_marker,
                    &cfg.github.bot_login,
                )
                .await;
                let publish_ms = publish_start.elapsed().as_millis() as u64;
                match publish_result {
                    // publish() records its own inline-review phase and
                    // refreshes the summary timing line
                    Ok(_) => {
                        let _ = state.save(&repo);
                    }
                    Err(err) => {
                        // keep whatever was marked posted before the failure,
                        // but never leave a reusable "complete" outcome behind
                        state.mark_publication_incomplete();
                        let _ = state.save(&repo);
                        // no publish phase recorded yet -> the failure
                        // happened before step 2 (e.g. head-sha re-check)
                        if !report.timing.phases.iter().any(|p| p.phase == "publish") {
                            report.timing.append_publish(
                                report.timing.total_ms,
                                publish_ms,
                                &format!(
                                    "error:{}",
                                    crate::text::excerpt_bytes(&err.to_string(), 60)
                                ),
                            );
                        }
                        report.plan.summary_markdown = crate::report::refresh_timing_line(
                            &report.plan.summary_markdown,
                            &report.timing,
                        );
                        eprintln!("error: publish failed: {err:#}");
                        report.status = RunStatus::Partial;
                        report.reason = Some(match report.reason.take() {
                            Some(r) => format!("{r}; publish failed: {err:#}"),
                            None => format!("publish failed: {err:#}"),
                        });
                        publish_failed = true;
                    }
                }
            }
            print!("{}", report.plan.summary_markdown);
            let out = a
                .out
                .unwrap_or_else(|| PathBuf::from(".revera/last-report.json"));
            if let Err(e) = write_report(&repo, &out, &report) {
                eprintln!("error: cannot write {}: {e:#}", out.display());
                return 1;
            }
            if !(api.is_some() && publish == PublishMode::Comment) {
                state.mark_posted(&surfaced_ids(&report));
                if let Err(e) = state.save(&repo) {
                    eprintln!("error: cannot save state: {e}");
                    return 1;
                }
            }
            eprintln!("report: {}", out.display());
            match report.status {
                _ if publish_failed => 2,
                RunStatus::Complete => 0,
                RunStatus::Partial => 2,
                RunStatus::Failed => 1,
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

/// Write the run report (redacted) without following symlinks under the
/// checkout. Relative paths resolve against the repository root.
fn write_report(
    repo: &std::path::Path,
    out: &std::path::Path,
    report: &crate::report::RunReport,
) -> anyhow::Result<()> {
    let mut v = serde_json::to_value(report)?;
    crate::redact::json(&mut v);
    crate::fsutil::write_output(repo, out, serde_json::to_string_pretty(&v)?.as_bytes())
}

/// Checks exactly what a review with this config would need: the routes
/// the configured strategy uses, Vera only when enabled, and the git repo.
/// Never prints credential values.
async fn doctor(
    config: Option<PathBuf>,
    profile: Option<String>,
    strategy: Option<StrategyArg>,
    publish: Option<PublishArg>,
) -> i32 {
    let mut ok = true;
    let strategy_override = strategy.map(strategy_arg);
    let overridden = strategy_override.is_some();
    // credentials are reported per-route below with `next:` hints; only
    // parse/shape/profile failures are config: FAIL
    let (path, cfg) = match load_cfg_unvalidated(config.as_deref(), profile.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            println!("config: FAIL — {e}");
            println!(
                "  next: fix the config (see revera.example.yaml and docs/configuration.md) or pass --config <path>"
            );
            return 1;
        }
    };
    println!("config: ok ({})", path.display());

    let strategy = strategy_override.unwrap_or(cfg.review.strategy);
    let mut notes = String::new();
    if overridden {
        notes.push_str(" (--strategy override)");
    }
    if let Some(p) = &profile {
        notes.push_str(&format!(" (profile {p})"));
    }
    println!(
        "strategy: {}{}",
        format!("{:?}", strategy).to_lowercase(),
        notes
    );
    if strategy != Strategy::Baseline {
        println!(
            "strategy: {:?} is advanced/experimental; baseline is the supported default",
            strategy
        );
    }

    let mut routes: Vec<(String, &crate::config::ModelRoute)> =
        vec![("investigator".into(), &cfg.models.investigator)];
    if !cfg.review.validate {
        println!("validator: DISABLED (review.validate=false; eval-only)");
    } else if cfg.validator_inherited() {
        println!("validator: inherited from investigator (same route, fresh context)");
    } else {
        routes.push(("validator".into(), cfg.effective_validator()));
    }
    match strategy {
        Strategy::Baseline => {}
        Strategy::Delegated => {
            if let Some(l) = &cfg.models.lead {
                routes.push(("models.lead".into(), l));
            }
            for (i, w) in cfg.models.workers.iter().flatten().enumerate() {
                routes.push((format!("models.workers[{i}]"), w));
            }
        }
        Strategy::Panel => {
            if let Err(e) = cfg.check_panel_lanes() {
                println!("panel: FAIL — {e}");
                println!("  next: configure one scout per panel.focuses entry (or a single scout)");
                ok = false;
            }
            for s in cfg.models.scouts.iter().flatten() {
                routes.push((format!("models.scouts.{}", s.name), &s.route));
            }
        }
    }
    for (name, r) in routes {
        if r.protocol.is_http() {
            let env = r.api_key_env.clone().unwrap_or_default();
            if env.is_empty() {
                println!("{name}: FAIL — api_key_env not set for an HTTP route");
                println!("  next: set api_key_env = \"YOUR_PROVIDER_KEY\" on the route");
                ok = false;
            } else if crate::config::env_is_set(&env) {
                println!(
                    "{name}: {} via {} (key env {env} set)",
                    r.model,
                    r.base_url.as_deref().unwrap_or("(default base url)")
                );
            } else {
                println!("{name}: FAIL — key env {env} missing or empty");
                println!(
                    "  next: export {env}=<your key> (or add it as a repository secret in CI)"
                );
                ok = false;
            }
            if r.reasoning.enabled() {
                println!(
                    "{name}: reasoning effort={} (budget {})",
                    r.reasoning.effort().as_str(),
                    r.reasoning.effective_budget()
                );
            }
        } else {
            match &r.script {
                Some(p) if p.exists() => println!("{name}: scripted route {}", p.display()),
                Some(p) => {
                    println!("{name}: FAIL — script {} not found", p.display());
                    ok = false;
                }
                None => {
                    println!("{name}: FAIL — scripted route without `script` path");
                    ok = false;
                }
            }
        }
    }

    if !cfg.vera.enabled {
        println!("vera: disabled (repository search is lexical-only; add a vera: block with an embedding endpoint to enable)");
    } else {
        match crate::vera::VeraClient::from_config(&cfg.vera, std::path::Path::new(".")) {
            Ok(v) => match v.version().await {
                Ok(ver) => match &cfg.vera.version {
                    Some(want) if want != &ver => {
                        println!("vera: version {ver} (config expects {want})")
                    }
                    _ => println!("vera: {ver}"),
                },
                Err(e) => {
                    println!("vera: FAIL — {e}");
                    println!(
                        "  next: install vera (scripts/install-vera.sh) or set vera.enabled = false"
                    );
                    ok = false;
                }
            },
            Err(e) => {
                println!("vera: FAIL — {e}");
                println!("  next: export the vera API key env var, or set vera.enabled = false");
                ok = false;
            }
        }
        if std::path::Path::new(".vera").exists() {
            println!(".vera index: present");
        } else {
            println!(".vera index: absent (built on first review)");
        }
    }
    let publish = publish.map(publish_arg).unwrap_or(cfg.review.publish);
    let publish_name = match publish {
        PublishMode::DryRun => "dry-run",
        PublishMode::Comment => "comment",
    };
    println!("publish: {publish_name}");
    if crate::config::env_is_set(&cfg.github.token_env) {
        println!("github: token env {} set", cfg.github.token_env);
    } else if publish == PublishMode::Comment {
        println!(
            "github: FAIL — token env {} missing or empty (needed for publish = comment)",
            cfg.github.token_env
        );
        println!(
            "  next: export {}=<token> or set review.publish = dry-run",
            cfg.github.token_env
        );
        ok = false;
    } else {
        println!(
            "github: token env {} not needed for dry-run",
            cfg.github.token_env
        );
    }
    if crate::git::is_repo(std::path::Path::new(".")).await {
        println!("git repo: yes");
    } else {
        println!("git repo: FAIL — current directory is not a git repository");
        ok = false;
    }
    println!("{}", if ok { "doctor: ok" } else { "doctor: FAIL" });
    if ok {
        0
    } else {
        1
    }
}

/// Prints the Vera index cache identity, or `disabled` when Vera is off so
/// callers (the Action) skip cache restore/save entirely.
async fn cache_key(
    config: Option<PathBuf>,
    profile: Option<String>,
    event: Option<PathBuf>,
    repo: PathBuf,
) -> i32 {
    let loaded = match event {
        Some(p) => match crate::github::event::parse(&p) {
            Ok(e) if crate::git::is_oid(&e.base_sha) => {
                let repo = crate::git::repo_root(&repo);
                load_event_config(&repo, config.as_deref(), profile.as_deref(), &e.base_sha)
                    .await
                    .map(|(_, c)| c)
            }
            Ok(_) => Err("event base is not a full commit id".into()),
            Err(e) => Err(format!("{e:#}")),
        },
        None => load_cfg_unvalidated(config.as_deref(), profile.as_deref()).map(|(_, c)| c),
    };
    match loaded {
        Ok(cfg) if !cfg.vera.enabled => {
            println!("disabled");
            0
        }
        Ok(cfg) => {
            use sha2::{Digest, Sha256};
            let id = cfg.vera.index_identity().to_string();
            let h = Sha256::digest(id.as_bytes());
            println!("{}", hex::encode(&h[..12]));
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn cache_info(repo: &std::path::Path) -> i32 {
    let p = crate::vera::VeraClient::cache_info_path(repo);
    match std::fs::read_to_string(&p) {
        Ok(s) => {
            println!("{s}");
            0
        }
        Err(_) => {
            eprintln!("no cache info at {}", p.display());
            1
        }
    }
}
