use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEvent {
    DiffParsed { files_changed: usize, bytes: usize },
    StaticRulesChecked { matches_found: usize },
    ScoutDispatched { lane: String, model: String },
    LaneCompleted { lane: String, candidates: usize, status: String },
    CandidatesAggregated { raw: usize, unique: usize, consensus: usize },
    ValidationStarted { count: usize },
    ValidationFinished { accepted: usize, rejected: usize, uncertain: usize },
    ReviewComplete { status: String, duration_ms: u64, published: usize },
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
