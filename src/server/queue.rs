use super::events::{Job, Kind};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Weak},
};
use tokio::sync::watch;

type Key = (u64, u64);

pub struct Running {
    pub job: Job,
    pub cancel: watch::Sender<bool>,
}

#[cfg(test)]
mod tests {
    use super::Queue;

    #[test]
    fn review_admissions_expire_and_close_invalidates_only_its_pr() {
        let mut queue = Queue::default();
        let first = queue.review_admission((1, 7));
        let concurrent = queue.review_admission((1, 7));
        let other = queue.review_admission((1, 8));
        queue.close((1, 7));
        assert!(!queue.admits((1, 7), &first));
        assert!(!queue.admits((1, 7), &concurrent));
        assert!(queue.admits((1, 8), &other));
        let reopened = queue.review_admission((1, 7));
        assert!(queue.admits((1, 7), &reopened));
        assert!(!queue.admits((1, 7), &first));
        drop(reopened);
        drop(other);
        for number in 9..1000 {
            let admission = queue.review_admission((1, number));
            assert!(queue.admits((1, number), &admission));
            assert_eq!(
                queue.admissions.len(),
                1,
                "expired lookups retained in memory"
            );
        }
    }
}

pub struct ReviewAdmission(Arc<()>);

#[derive(Default)]
pub struct Queue {
    pub pending: BTreeMap<Key, Job>,
    pub running: BTreeMap<Key, Running>,
    // Only in-flight lookups keep tokens alive. Expired entries are pruned when
    // admitting the next lookup, rather than retaining every PR ever closed.
    admissions: BTreeMap<Key, Weak<()>>,
}

impl Queue {
    pub fn review_admission(&mut self, key: Key) -> ReviewAdmission {
        self.admissions.retain(|_, token| token.strong_count() > 0);
        let token = self.admissions.entry(key).or_default();
        let current = token.upgrade().unwrap_or_else(|| {
            let current = Arc::new(());
            *token = Arc::downgrade(&current);
            current
        });
        ReviewAdmission(current)
    }

    pub fn admits(&self, key: Key, admission: &ReviewAdmission) -> bool {
        self.admissions
            .get(&key)
            .is_some_and(|current| current.ptr_eq(&Arc::downgrade(&admission.0)))
    }

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
        self.admissions.remove(&key);
        self.pending.remove(&key);
        if let Some(running) = self.running.get(&key) {
            let _ = running.cancel.send(true);
        }
    }

    pub fn remove_repo(&mut self, repo: super::events::Repo) -> Vec<Job> {
        self.admissions.retain(|key, _| key.0 != repo.id);
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
            mode: super::dashboard::Mode::Standard,
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
