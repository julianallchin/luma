//! Live, account-scoped progress. Counters describe the current phase, not an
//! invented total for remote work that has not been discovered yet.
use std::sync::Mutex;

use crate::models::sync::SyncProgress;

#[derive(Default)]
pub struct Progress(Mutex<Option<(String, SyncProgress)>>);

impl Progress {
    pub fn start(&self, uid: &str) -> Run<'_> {
        *self.0.lock().unwrap() = Some((
            uid.into(),
            SyncProgress {
                phase: "Preparing sync".into(),
                completed: 0,
                total: None,
                unit: "items".into(),
            },
        ));
        Run(self)
    }

    pub fn snapshot(&self, uid: &str) -> Option<SyncProgress> {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(owner, _)| owner == uid)
            .map(|(_, progress)| progress.clone())
    }

    pub fn phase(&self, phase: impl Into<String>, total: Option<usize>, unit: &str) {
        if let Some((_, progress)) = self.0.lock().unwrap().as_mut() {
            *progress = SyncProgress {
                phase: phase.into(),
                completed: 0,
                total,
                unit: unit.into(),
            };
        }
    }

    /// Count processed items, including failed or skipped attempts. Dropping on
    /// every branch keeps `continue`, errors and cancellation from losing ticks.
    pub fn item(&self) -> Item<'_> {
        Item(self)
    }
}

pub struct Run<'a>(&'a Progress);
impl Drop for Run<'_> {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap() = None;
    }
}

pub struct Item<'a>(&'a Progress);
impl Drop for Item<'_> {
    fn drop(&mut self) {
        if let Some((_, progress)) = self.0 .0.lock().unwrap().as_mut() {
            progress.completed += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_counts_attempts_resets_phases_and_clears_on_exit() {
        let progress = Progress::default();
        let run = progress.start("alice");
        assert!(progress.snapshot("bob").is_none());
        progress.phase("Uploading audio", Some(3), "files");
        {
            let _item = progress.item();
        }
        assert_eq!(progress.snapshot("alice").unwrap().completed, 1);
        progress.phase("Checking cloud changes", None, "items");
        let snapshot = progress.snapshot("alice").unwrap();
        assert_eq!(snapshot.completed, 0);
        assert_eq!(snapshot.total, None);
        drop(run);
        assert!(progress.snapshot("alice").is_none());
    }
}
