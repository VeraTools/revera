use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEvent {
    DiffParsed {
        files_changed: usize,
        bytes: usize,
    },
    StaticRulesChecked {
        matches_found: usize,
    },
    ScoutDispatched {
        lane: String,
        model: String,
    },
    LaneCompleted {
        lane: String,
        candidates: usize,
        status: String,
    },
    CandidatesAggregated {
        raw: usize,
        unique: usize,
        consensus: usize,
    },
    ValidationStarted {
        count: usize,
    },
    ValidationFinished {
        accepted: usize,
        rejected: usize,
        uncertain: usize,
    },
    ReviewComplete {
        status: String,
        duration_ms: u64,
        published: usize,
    },
}

impl ProgressEvent {
    /// Formats the progress event as a single-line JSON string.
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(Clone, Debug)]
pub struct ProgressBroadcaster {
    sender: broadcast::Sender<ProgressEvent>,
}

impl ProgressBroadcaster {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProgressEvent> {
        self.sender.subscribe()
    }

    pub fn emit(&self, event: ProgressEvent) {
        tracing::info!(target: "revera::progress", "{:?}", &event);
        let _ = self.sender.send(event);
    }
}

impl Default for ProgressBroadcaster {
    fn default() -> Self {
        Self::new(128)
    }
}

/// Stream every event emitted on `b` as one JSON object per line to `path`
/// (`-` for stderr) until every sender is dropped. The subscription is taken
/// before this returns, so no event emitted afterwards is missed; if the
/// writer falls behind, a `lagged` line says how many events were skipped.
pub fn spawn_ndjson_writer(
    b: &ProgressBroadcaster,
    path: &std::path::Path,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    use std::io::Write;
    let mut out: Box<dyn Write + Send> = if path.as_os_str() == "-" {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::fs::File::create(path)?)
    };
    let mut rx = b.subscribe();
    Ok(tokio::spawn(async move {
        loop {
            let line = match rx.recv().await {
                Ok(ev) => ev.to_json_line(),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    serde_json::json!({"event": "lagged", "skipped": n}).to_string()
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                tracing::warn!("progress stream closed by the reader; events dropped");
                break;
            }
        }
    }))
}
