//! Wall-clock phase timing for a review run.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One timed phase. `phase` is one of "vera_index" | "recheck" | "lane" |
/// "plan" | "synthesis" | "arbitration" | "validate" | "publish". `label` is
/// the lane/route label ("investigator", "panel:<focus>", "worker:<qid>",
/// candidate id for validate, "" otherwise). `outcome` is "ok" (or
/// "ok:candidates"/"ok:accepted" variants), "timeout", "tool_budget",
/// "error:<short>", or "skipped".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseTiming {
    pub phase: String,
    pub label: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    /// Time spent queued (e.g. waiting on the concurrency semaphore) before
    /// execution started; 0 for non-validate phases.
    #[serde(default)]
    pub queue_ms: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Timing {
    pub total_ms: u64,
    pub vera_index_ms: Option<u64>,
    /// End of the first lane that returned >=1 candidate.
    pub first_candidate_ms: Option<u64>,
    /// End of the first validate phase with an accepted verdict.
    pub first_validated_ms: Option<u64>,
    /// Wall from first lane start to last lane end.
    pub lanes_ms: Option<u64>,
    /// Wall from first validate start to last validate end.
    pub validate_ms: Option<u64>,
    pub validate_p50_ms: Option<u64>,
    pub validate_p95_ms: Option<u64>,
    /// Phases whose outcome is neither ok nor skipped.
    pub incomplete_phases: u32,
    /// Phases whose outcome is "skipped".
    #[serde(default)]
    pub skipped_phases: u32,
    pub phases: Vec<PhaseTiming>,
}

impl Timing {
    /// Record a publish phase after `finish()` (cli measures publication
    /// itself, after the report's timing was already finalized).
    pub fn append_publish(&mut self, start_ms: u64, duration_ms: u64, outcome: &str) {
        if !is_ok(outcome) && outcome != "skipped" {
            self.incomplete_phases += 1;
        }
        if outcome == "skipped" {
            self.skipped_phases += 1;
        }
        self.phases.push(PhaseTiming {
            phase: "publish".into(),
            label: String::new(),
            start_ms,
            duration_ms,
            queue_ms: 0,
            outcome: outcome.into(),
        });
        self.total_ms += duration_ms;
    }
}

fn is_ok(outcome: &str) -> bool {
    outcome.starts_with("ok")
}

/// Nearest-rank percentile over `durations` (ms).
fn percentile(durations: &[u64], p: f64) -> Option<u64> {
    if durations.is_empty() {
        return None;
    }
    let mut d = durations.to_vec();
    d.sort_unstable();
    let rank = ((p / 100.0) * d.len() as f64).ceil() as usize;
    Some(d[rank.max(1) - 1])
}

fn phase_end(p: &PhaseTiming) -> u64 {
    p.start_ms + p.duration_ms
}

/// Shared recorder; cheap to clone into spawned tasks.
#[derive(Clone, Default)]
pub struct Recorder(Arc<Mutex<Vec<PhaseTiming>>>);

impl Recorder {
    /// Record a phase that began at `start` (relative to run start `wall`,
    /// which is also `Instant::now()` at run start).
    pub fn record(&self, phase: &str, label: &str, start: Instant, wall: Instant, outcome: &str) {
        self.record_queued(phase, label, start, 0, wall, outcome);
    }

    /// Record a phase that began executing at `exec_start` after waiting
    /// `queue_ms` (e.g. on the validate semaphore).
    pub fn record_queued(
        &self,
        phase: &str,
        label: &str,
        exec_start: Instant,
        queue_ms: u64,
        wall: Instant,
        outcome: &str,
    ) {
        let p = PhaseTiming {
            phase: phase.into(),
            label: label.into(),
            start_ms: exec_start.saturating_duration_since(wall).as_millis() as u64,
            duration_ms: exec_start.elapsed().as_millis() as u64,
            queue_ms,
            outcome: outcome.into(),
        };
        self.0.lock().unwrap().push(p);
    }

    pub fn finish(&self, wall: Instant) -> Timing {
        let mut phases = self.0.lock().unwrap().clone();
        phases.sort_by_key(|p| p.start_ms);
        let span = |name: &str| -> Option<u64> {
            let ps: Vec<&PhaseTiming> = phases
                .iter()
                .filter(|p| p.phase == name && p.outcome != "skipped")
                .collect();
            if ps.is_empty() {
                return None;
            }
            let first = ps.iter().map(|p| p.start_ms).min().unwrap();
            let last = ps.iter().map(|p| phase_end(p)).max().unwrap();
            Some(last - first)
        };
        let validate_durations: Vec<u64> = phases
            .iter()
            .filter(|p| p.phase == "validate" && p.outcome != "skipped")
            .map(|p| p.duration_ms)
            .collect();
        Timing {
            total_ms: wall.elapsed().as_millis() as u64,
            vera_index_ms: phases
                .iter()
                .find(|p| p.phase == "vera_index")
                .map(|p| p.duration_ms),
            first_candidate_ms: phases
                .iter()
                .filter(|p| p.phase == "lane" && p.outcome == "ok:candidates")
                .map(phase_end)
                .min(),
            first_validated_ms: phases
                .iter()
                .filter(|p| p.phase == "validate" && p.outcome == "ok:accepted")
                .map(phase_end)
                .min(),
            lanes_ms: span("lane"),
            validate_ms: span("validate"),
            validate_p50_ms: percentile(&validate_durations, 50.0),
            validate_p95_ms: percentile(&validate_durations, 95.0),
            incomplete_phases: phases
                .iter()
                .filter(|p| !is_ok(&p.outcome) && p.outcome != "skipped")
                .count() as u32,
            skipped_phases: phases.iter().filter(|p| p.outcome == "skipped").count() as u32,
            phases,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(phase: &str, label: &str, start_ms: u64, dur: u64, outcome: &str) -> PhaseTiming {
        PhaseTiming {
            phase: phase.into(),
            label: label.into(),
            start_ms,
            duration_ms: dur,
            queue_ms: 0,
            outcome: outcome.into(),
        }
    }

    fn timing_of(phases: Vec<PhaseTiming>) -> Timing {
        let r = Recorder::default();
        for p in phases {
            r.0.lock().unwrap().push(p);
        }
        // wall: measure against a synthetic start; finish() uses real elapsed
        // for total_ms, so just call it and ignore total.
        r.finish(Instant::now())
    }

    #[test]
    fn derived_metrics() {
        let t = timing_of(vec![
            phase("vera_index", "", 0, 4000, "ok"),
            phase("lane", "panel:general", 4000, 30000, "ok:candidates"),
            phase("lane", "panel:cross", 4000, 20000, "ok"),
            phase("validate", "c1", 34000, 6000, "ok:accepted"),
            phase("validate", "c2", 35000, 8000, "ok"),
        ]);
        assert_eq!(t.vera_index_ms, Some(4000));
        assert_eq!(t.first_candidate_ms, Some(34000));
        assert_eq!(t.first_validated_ms, Some(40000));
        assert_eq!(t.lanes_ms, Some(30000));
        assert_eq!(t.validate_ms, Some(9000));
        assert_eq!(t.incomplete_phases, 0);
        assert_eq!(t.phases.len(), 5);
    }

    #[test]
    fn percentiles_and_incomplete() {
        let mut v = vec![];
        for (i, d) in [1000u64, 2000, 3000, 4000, 5000].iter().enumerate() {
            v.push(phase(
                "validate",
                &format!("c{i}"),
                i as u64 * 6000,
                *d,
                "ok",
            ));
        }
        v.push(phase("lane", "l", 0, 1000, "timeout"));
        v.push(phase("lane", "l2", 50000, 0, "skipped"));
        let t = timing_of(v);
        assert_eq!(t.validate_p50_ms, Some(3000));
        assert_eq!(t.validate_p95_ms, Some(5000));
        assert_eq!(t.incomplete_phases, 1); // skipped does not count
        assert_eq!(t.skipped_phases, 1);
        // the skipped lane extends neither the lane span nor percentiles
        assert_eq!(t.lanes_ms, Some(1000));
    }

    #[test]
    fn skipped_validate_excluded_from_spans_and_percentiles() {
        let t = timing_of(vec![
            phase("validate", "c1", 0, 1000, "ok"),
            phase("validate", "c2", 1000, 2000, "ok"),
            phase("validate", "c3", 2000, 3000, "ok"),
            phase("validate", "c4", 9000, 0, "skipped"),
            phase("validate", "c5", 9500, 0, "skipped"),
        ]);
        assert_eq!(t.validate_p50_ms, Some(2000));
        assert_eq!(t.validate_p95_ms, Some(3000));
        assert_eq!(t.validate_ms, Some(5000));
        assert_eq!(t.skipped_phases, 2);
        assert_eq!(t.incomplete_phases, 0);
    }

    #[test]
    fn record_queued_stores_queue_and_exec_start() {
        let r = Recorder::default();
        let wall = Instant::now() - std::time::Duration::from_secs(1);
        let exec = Instant::now();
        r.record_queued("validate", "c1", exec, 250, wall, "ok");
        let t = r.finish(wall);
        let p = &t.phases[0];
        assert_eq!(p.queue_ms, 250);
        // start_ms is measured from exec_start, not from when the task queued
        assert!(p.start_ms >= 1000, "{}", p.start_ms);
    }

    #[test]
    fn empty_is_none() {
        let t = timing_of(vec![]);
        assert!(t.lanes_ms.is_none() && t.validate_ms.is_none());
        assert_eq!(t.incomplete_phases, 0);
    }

    #[test]
    fn append_publish() {
        let mut t = Timing::default();
        t.append_publish(1000, 250, "ok");
        assert_eq!(t.total_ms, 250);
        assert_eq!(t.phases.len(), 1);
        t.append_publish(1250, 10, "error:403");
        assert_eq!(t.incomplete_phases, 1);
    }
}
