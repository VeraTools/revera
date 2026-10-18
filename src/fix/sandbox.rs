//! Linux bubblewrap boundary. No /proc or devpts dependency; no fallback to
//! unsandboxed execution. The caller supplies only dedicated ephemeral binds.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// The complete environment of an exec'd child before profile-specific model
/// credentials. PWD is also maintained by bubblewrap's --chdir. A test shell may
/// add its own shell bookkeeping; isolation tests exec /usr/bin/env directly.
pub const FIXED_ENV: &[(&str, &str)] = &[
    ("PATH", "/usr/local/bin:/usr/bin:/bin"),
    ("HOME", "/tmp"),
    ("TMPDIR", "/tmp"),
    ("LC_ALL", "C.UTF-8"),
    ("PWD", "/work"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxStatus {
    pub available: bool,
    pub proc: bool,
    pub reason: Option<String>,
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sandbox unavailable: {0}")]
    Unavailable(String),
    #[error("invalid sandbox profile: {0}")]
    Profile(String),
    #[error("sandbox I/O: {0}")]
    Io(#[from] std::io::Error),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    pub cpu_s: u64,
    pub mem_bytes: u64,
    pub fsize: u64,
    pub nproc: u64,
    pub wall: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu_s: 300,
            mem_bytes: 2 * 1024 * 1024 * 1024,
            fsize: 16 * 1024 * 1024,
            nproc: 128,
            wall: Duration::from_secs(360),
        }
    }
}
#[derive(Clone, Default)]
pub struct Profile {
    pub net: bool,
    pub binds_rw: Vec<(PathBuf, PathBuf)>,
    pub binds_ro: Vec<(PathBuf, PathBuf)>,
    /// Explicit variables only. Do not put GitHub credentials here.
    pub env: Vec<(String, String)>,
    pub limits: Limits,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Output {
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

fn base(proc: bool, net: bool) -> Command {
    let mut c = Command::new("/usr/bin/bwrap");
    c.env_clear()
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
    ]);
    if Path::new("/usr/lib64").exists() {
        c.args(["--symlink", "usr/lib64", "/lib64"]);
    }
    c.args([
        "--dev-bind",
        "/dev/null",
        "/dev/null",
        "--dev-bind",
        "/dev/urandom",
        "/dev/urandom",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/work",
    ]);
    if proc {
        c.args(["--proc", "/proc"]);
    }
    if net {
        c.arg("--share-net");
        for p in [
            "/etc/ssl",
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
        ] {
            if Path::new(p).exists() {
                c.args(["--ro-bind", p, p]);
            }
        }
    }
    for (name, value) in FIXED_ENV {
        c.args(["--setenv", name, value]);
    }
    c.args(["--chdir", "/work"]);
    c
}

pub async fn probe() -> SandboxStatus {
    if !cfg!(target_os = "linux")
        || !Path::new("/usr/bin/bwrap").is_file()
        || !Path::new("/usr/bin/prlimit").is_file()
    {
        return SandboxStatus {
            available: false,
            proc: false,
            reason: Some("Linux, /usr/bin/bwrap and /usr/bin/prlimit are required".into()),
        };
    }
    let mut reason = String::new();
    for proc in [true, false] {
        let mut c = base(proc, false);
        c.args([
            "--remount-ro",
            "/",
            "--",
            "/usr/bin/prlimit",
            "--cpu=2:2",
            "--as=268435456:268435456",
            "--fsize=1048576:1048576",
            "--nproc=32:32",
            "--",
            "/usr/bin/true",
        ]);
        match tokio::time::timeout(Duration::from_secs(5), c.output()).await {
            Ok(Ok(o)) if o.status.success() => {
                return SandboxStatus {
                    available: true,
                    proc,
                    reason: (!proc)
                        .then(|| format!("/proc unavailable; using proc-free isolation: {reason}")),
                };
            }
            Ok(Ok(o)) => {
                reason = crate::redact::diagnostic(&crate::text::excerpt_bytes(
                    &String::from_utf8_lossy(&o.stderr),
                    1000,
                ))
            }
            Ok(Err(e)) => reason = e.to_string(),
            Err(_) => reason = "probe timed out".into(),
        }
    }
    SandboxStatus {
        available: false,
        proc: false,
        reason: Some(reason),
    }
}

pub fn command(status: &SandboxStatus, p: &Profile, argv: &[String]) -> Result<Command, Error> {
    if !status.available {
        return Err(Error::Unavailable(
            status.reason.clone().unwrap_or_default(),
        ));
    }
    if argv.is_empty()
        || p.limits.wall.is_zero()
        || p.limits.cpu_s == 0
        || p.limits.mem_bytes == 0
        || p.limits.nproc == 0
        || p.limits.fsize == 0
    {
        return Err(Error::Profile("empty command or zero limits".into()));
    }
    let mut c = base(status.proc, p.net);
    for (rw, pairs) in [(false, &p.binds_ro), (true, &p.binds_rw)] {
        for (host, inside) in pairs {
            let name = inside
                .to_str()
                .ok_or_else(|| Error::Profile("non UTF-8 destination".into()))?;
            if !inside.is_absolute()
                || inside
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                || !["/work", "/input", "/output", "/revera"]
                    .iter()
                    .any(|r| name == *r || name.starts_with(&format!("{r}/")))
            {
                return Err(Error::Profile(
                    "bind destinations must be under /work, /input, /output or /revera".into(),
                ));
            }
            let meta = std::fs::symlink_metadata(host)?;
            if meta.file_type().is_symlink() || (!meta.is_file() && !meta.is_dir()) {
                return Err(Error::Profile(
                    "bind source must be a real file or directory, never a socket/symlink".into(),
                ));
            }
            c.arg(if rw { "--bind" } else { "--ro-bind" })
                .arg(host)
                .arg(inside);
        }
    }
    for (k, v) in &p.env {
        if !crate::config::is_env_name(k)
            || matches!(
                k.as_str(),
                "GITHUB_TOKEN"
                    | "GH_TOKEN"
                    | "ACTIONS_RUNTIME_TOKEN"
                    | "LD_PRELOAD"
                    | "LD_LIBRARY_PATH"
            )
        {
            return Err(Error::Profile("forbidden environment variable".into()));
        }
        c.args(["--setenv", k, v]);
    }
    c.args(["--remount-ro", "/", "--", "/usr/bin/prlimit"]);
    for (name, limit) in [
        ("cpu", p.limits.cpu_s),
        ("as", p.limits.mem_bytes),
        ("fsize", p.limits.fsize),
        ("nproc", p.limits.nproc),
        ("core", 0),
    ] {
        c.arg(format!("--{name}={limit}:{limit}"));
    }
    c.arg("--").args(argv);
    Ok(c)
}

async fn capture(mut pipe: impl AsyncRead + Unpin, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = pipe.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let room = cap.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
        truncated |= n > room;
    }
    Ok((kept, truncated))
}
pub async fn run(status: &SandboxStatus, p: &Profile, argv: &[String]) -> Result<Output, Error> {
    let mut child = command(status, p, argv)?.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let task = async {
        let (exit, out, err) = tokio::try_join!(
            child.wait(),
            capture(stdout, 32 * 1024),
            capture(stderr, 32 * 1024)
        )?;
        Ok::<_, std::io::Error>((exit, out, err))
    };
    match tokio::time::timeout(p.limits.wall, task).await {
        Ok(result) => {
            let (exit, (out, ot), (err, et)) = result?;
            Ok(Output {
                code: exit.code(),
                timed_out: false,
                stdout: crate::text::excerpt_bytes(
                    &crate::redact::text(&String::from_utf8_lossy(&out)),
                    32 * 1024,
                ),
                stderr: crate::text::excerpt_bytes(
                    &crate::redact::text(&String::from_utf8_lossy(&err)),
                    32 * 1024,
                ),
                truncated: ot || et,
            })
        }
        Err(_) => {
            child.kill().await?;
            Ok(Output {
                code: None,
                timed_out: true,
                stdout: String::new(),
                stderr: "wall-clock limit exceeded".into(),
                truncated: false,
            })
        }
    }
}
