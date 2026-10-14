use super::events::{Job, Kind};
use std::collections::{BTreeMap, HashSet};
use tokio::sync::watch;

type Key = (u64, u64);

pub struct Running {
    pub job: Job,
    pub cancel: watch::Sender<bool>,
}

#[derive(Default)]
pub struct Queue {
    pub pending: BTreeMap<Key, Job>,
    pub running: BTreeMap<Key, Running>,
}

impl Queue {
    pub fn enqueue(&mut self, job: Job) {
        let key = job.key();
        if let Some(running) = self.running.get(&key) {
            if matches!(job.kind, Kind::Index) {
                self.pending.insert(key, job);
                return;
            }
            if running.job.head() == job.head()
                && matches!(job.kind, Kind::Review { force: true, .. })
            {
                self.pending.insert(key, job);
                return;
            }
            if running.job.head() == job.head()
                && !matches!(job.kind, Kind::Review { force: true, .. } | Kind::Remove)
            {
                return;
            }
            let _ = running.cancel.send(true);
        }
        self.pending.insert(key, job);
    }

    pub fn close(&mut self, key: Key) {
        self.pending.remove(&key);
        if let Some(running) = self.running.get(&key) {
            let _ = running.cancel.send(true);
        }
    }

    pub fn remove_repo(&mut self, repo: super::events::Repo) -> Vec<Job> {
        let cancelled = self
            .pending
            .values()
            .chain(self.running.values().map(|running| &running.job))
            .filter(|job| job.repo.id == repo.id)
            .cloned()
            .collect();
        self.pending.retain(|key, _| key.0 != repo.id);
        for (key, running) in &self.running {
            if key.0 == repo.id {
                let _ = running.cancel.send(true);
            }
        }
        self.enqueue(Job {
            id: uuid::Uuid::new_v4().simple().to_string(),
            trigger: "webhook".into(),
            repo,
            kind: Kind::Remove,
        });
        cancelled
    }

    pub fn contains(&self, key: Key) -> bool {
        self.pending.contains_key(&key) || self.running.contains_key(&key)
    }

    pub fn take(&mut self) -> Option<(Job, watch::Receiver<bool>)> {
        let busy: HashSet<_> = self.running.keys().map(|key| key.0).collect();
        let key = *self.pending.keys().find(|key| !busy.contains(&key.0))?;
        let job = self.pending.remove(&key)?;
        let (cancel, rx) = watch::channel(false);
        self.running.insert(
            key,
            Running {
                job: job.clone(),
                cancel,
            },
        );
        Some((job, rx))
    }
}
