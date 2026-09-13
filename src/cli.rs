use crate::config::{Config, PublishMode, Strategy};
use crate::pipeline::baseline::{run as baseline_run, ReviewRequest};
use crate::report::RunStatus;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "revera",
    about = "Provider-independent PR reviewer with Vera retrieval"
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
    let mut publish = a
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
    let mut fork_note = false;
    if let Some(e) = &ev {
        if e.is_fork() && publish == PublishMode::Comment && !cfg.github.allow_forks {
            eprintln!(
                "revera: fork PR ({} -> {}): publication skipped (github.allow_forks=false)",
                e.head_repo_full_name, e.repo_full_name
            );
            publish = PublishMode::DryRun;
            fork_note = true;
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
    let mut prior_open_titles: Vec<String> = Vec::new();
    let (base, head, title, pr_body) = match &api {
        Some((e, api)) => {
            // ensure the base sha exists locally (shallow checkouts)
            if !crate::git::has_commit(&repo, &e.base_sha).await {
                if let Err(err) = crate::git::fetch_sha(&repo, &e.base_sha).await {
                    eprintln!("error: cannot fetch base sha {}: {err:#}", e.base_sha);
                    return 1;
                }
            }
            // seed state from the managed summary comment (fallback: empty)
            let (owner, rname) = e.owner_repo();
            let seeded = match api.list_issue_comments(owner, rname, e.number).await {
                Ok(comments) => comments
                    .iter()
                    .find(|c| c.body.contains(&cfg.github.summary_marker))
                    .and_then(|c| crate::github::publish::decode_state(&c.body))
                    .unwrap_or_default(),
                Err(err) => {
                    tracing::warn!("could not list PR comments for state: {err:#}");
                    crate::state::ReviewState::default()
                }
            };
            prior_open_titles = seeded
                .open_findings()
                .iter()
                .map(|f| f.title.clone())
                .collect();
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
    match baseline_run(&cfg, &req).await {
        Ok((mut report, mut state)) => {
            if fork_note {
                report
                    .plan
                    .summary_markdown
                    .push_str("\n> fork PR: publication skipped\n");
                report.publication.mode = "dry-run".into();
                report.publication.skipped_reason = Some("fork PR: publication skipped".into());
            }
            let mut publish_failed = false;
            if let (Some((e, api)), PublishMode::Comment) = (&api, publish) {
                let resolved: Vec<String> = state
                    .findings
                    .iter()
                    .filter(|f| {
                        f.status == crate::state::FindingState::Resolved
                            && prior_open_titles.contains(&f.title)
                    })
                    .map(|f| f.title.clone())
                    .collect();
                match crate::github::publish::publish(
                    api,
                    e,
                    &mut report,
                    &mut state,
                    cfg.review.max_findings,
                    &cfg.github.summary_marker,
                    &resolved,
                )
                .await
                {
                    Ok(_) => {
                        let _ = state.save(&repo);
                    }
                    Err(err) => {
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

async fn doctor(config: Option<PathBuf>) -> i32 {
    let mut ok = true;
    let cfg = match load_cfg(config.as_deref(), None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config: FAIL — {e}");
            return 1;
        }
    };
    println!("config: ok");
    for (name, r) in [
        ("investigator", &cfg.models.investigator),
        ("validator", &cfg.models.validator),
    ] {
        if r.protocol == crate::config::Protocol::OpenaiChat {
            let env = r.api_key_env.clone().unwrap_or_default();
            if std::env::var(&env).is_ok() {
                println!("models.{name}: key env {env} set");
            } else {
                println!("models.{name}: FAIL — key env {env} not set");
                ok = false;
            }
        }
    }
    match crate::vera::VeraClient::from_config(&cfg.vera, std::path::Path::new(".")) {
        Ok(v) => match v.version().await {
            Ok(ver) => {
                if let Some(want) = &cfg.vera.version {
                    if &ver != want {
                        println!("vera: version {ver} (config expects {want})");
                    } else {
                        println!("vera: {ver}");
                    }
                } else {
                    println!("vera: {ver}");
                }
            }
            Err(e) => {
                println!("vera: FAIL — {e}");
                ok = false;
            }
        },
        Err(e) => {
            println!("vera: FAIL — {e}");
            ok = false;
        }
    }
    if crate::git::is_repo(std::path::Path::new(".")).await {
        println!("git repo: yes");
    } else {
        println!("git repo: no");
        ok = false;
    }
    if std::path::Path::new(".vera").exists() {
        println!(".vera index: present");
    } else {
        println!(".vera index: absent (will be created on first review)");
    }
    if ok {
        0
    } else {
        1
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
