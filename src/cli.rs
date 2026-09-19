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
        Cmd::Doctor { config } => doctor(config).await,
        Cmd::CacheInfo { repo } => cache_info(&repo),
        Cmd::CacheKey { config, profile } => cache_key(config, profile),
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

fn load_cfg(path: Option<&std::path::Path>, profile: Option<&str>) -> Result<Config, String> {
    let p = Config::find(path).map_err(|e| e.to_string())?;
    let mut c = Config::load(&p).map_err(|e| e.to_string())?;
    if let Some(pr) = profile {
        c.apply_profile(pr).map_err(|e| e.to_string())?;
    }
    Ok(c)
}

async fn review(a: ReviewArgs) -> i32 {
    let cfg = match load_cfg(a.config.as_deref(), a.profile.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let publish = a
        .publish
        .map(|p| match p {
            PublishArg::DryRun => PublishMode::DryRun,
            PublishArg::Comment => PublishMode::Comment,
        })
        .unwrap_or(cfg.review.publish);
    if publish == PublishMode::Comment && a.event.is_none() {
        eprintln!("error: --publish comment requires --event");
        return 1;
    }

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
    // Fork guard: no model or Vera calls at all — return partial immediately.
    if let Some(e) = &ev {
        if e.is_fork() && publish == PublishMode::Comment && !cfg.github.allow_forks {
            let reason = "fork PR: review skipped (github.allow_forks=false)";
            eprintln!(
                "revera: {reason} ({} -> {})",
                e.head_repo_full_name, e.repo_full_name
            );
            let rep = crate::report::RunReport {
                status: crate::report::RunStatus::Partial,
                reason: Some(reason.into()),
                base: e.base_sha.clone(),
                head: e.head_sha.clone(),
                strategy: format!("{:?}", cfg.review.strategy).to_lowercase(),
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
                .unwrap_or_else(|| crate::git::repo_root(&a.repo).join(".revera/last-report.json"));
            if let Some(p) = out.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let _ = std::fs::write(&out, serde_json::to_string_pretty(&rep).unwrap());
            eprintln!("report: {}", out.display());
            return 2;
        }
    }
    let strategy = a.strategy.map(|s| match s {
        StrategyArg::Baseline => Strategy::Baseline,
        StrategyArg::Delegated => Strategy::Delegated,
        StrategyArg::Panel => Strategy::Panel,
    });
    let body = a
        .body_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let repo = crate::git::repo_root(&a.repo);

    // Event mode supplies base/head/title/body and GitHub-backed state.
    let api = ev.as_ref().map(|e| {
        let token = std::env::var(&cfg.github.token_env).unwrap_or_default();
        (e.clone(), crate::github::api::GitHubApi::new(&token))
    });
    let (base, head, title, pr_body) = match &api {
        Some((e, api)) => {
            // ensure the base sha exists locally (shallow checkouts)
            if !crate::git::has_commit(&repo, &e.base_sha).await {
                if let Err(err) = crate::git::fetch_sha(&repo, &e.base_sha).await {
                    eprintln!("error: cannot fetch base sha {}: {err:#}", e.base_sha);
                    return 1;
                }
            }
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
                .unwrap_or_else(|| repo.join(".revera/last-report.json"));
            if let Some(parent) = out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap()) {
                eprintln!("error: cannot write {}: {e}", out.display());
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

/// Checks exactly what a review with this config would need: the routes
/// the configured strategy uses, Vera only when enabled, and the git repo.
/// Never prints credential values.
async fn doctor(config: Option<PathBuf>) -> i32 {
    let mut ok = true;
    let cfg = match load_cfg(config.as_deref(), None) {
        Ok(c) => c,
        Err(e) => {
            println!("config: FAIL — {e}");
            println!("  next: fix revera.yaml (see revera.example.yaml) or pass --config <path>");
            return 1;
        }
    };
    println!(
        "config: ok (strategy {}, publish {:?}, validate {})",
        format!("{:?}", cfg.review.strategy).to_lowercase(),
        cfg.review.publish,
        cfg.review.validate
    );

    let mut routes: Vec<(String, &crate::config::ModelRoute)> =
        vec![("models.investigator".into(), &cfg.models.investigator)];
    if cfg.review.validate {
        routes.push(("models.validator".into(), &cfg.models.validator));
    }
    match cfg.review.strategy {
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
            for s in cfg.models.scouts.iter().flatten() {
                routes.push((format!("models.scouts.{}", s.name), &s.route));
            }
        }
    }
    if cfg.review.strategy != Strategy::Baseline {
        println!(
            "strategy: {:?} is advanced/experimental; baseline is the supported default",
            cfg.review.strategy
        );
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
        println!("vera: disabled (repository-wide lookups use lexical search only)");
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
    if cfg.review.publish == PublishMode::Comment {
        if crate::config::env_is_set(&cfg.github.token_env) {
            println!("github: token env {} set", cfg.github.token_env);
        } else {
            println!(
                "github: FAIL — token env {} missing or empty (needed for publish = comment)",
                cfg.github.token_env
            );
            println!(
                "  next: export {}=<token> or set review.publish = dry-run",
                cfg.github.token_env
            );
            ok = false;
        }
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
fn cache_key(config: Option<PathBuf>, profile: Option<String>) -> i32 {
    match load_cfg(config.as_deref(), profile.as_deref()) {
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
