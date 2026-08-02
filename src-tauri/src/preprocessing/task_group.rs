use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::async_runtime::JoinHandle;
use tokio::sync::Notify;

const IDENTITY_CANCELLED: &str = "Audio analysis was cancelled by an identity transition";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisEpoch(u64);

#[derive(Clone, Debug)]
pub struct AnalysisGuard {
    cancelled: Arc<AtomicBool>,
}

impl AnalysisGuard {
    pub fn checkpoint(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(IDENTITY_CANCELLED.into())
        } else {
            Ok(())
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

struct TaskGroupState {
    generation: u64,
    accepting: bool,
    active: usize,
    cancelled: Arc<AtomicBool>,
}

struct TaskGroupInner {
    state: Mutex<TaskGroupState>,
    drained: Notify,
}

/// Owns every background audio-analysis task admitted for one host identity.
/// Epochs close the gap between a command beginning and its background work
/// being spawned; leases keep the identity boundary open until all children
/// have stopped publishing.
#[derive(Clone)]
pub struct AnalysisTaskGroup {
    inner: Arc<TaskGroupInner>,
}

impl Default for AnalysisTaskGroup {
    fn default() -> Self {
        Self {
            inner: Arc::new(TaskGroupInner {
                state: Mutex::new(TaskGroupState {
                    generation: 0,
                    accepting: true,
                    active: 0,
                    cancelled: Arc::new(AtomicBool::new(false)),
                }),
                drained: Notify::new(),
            }),
        }
    }
}

impl AnalysisTaskGroup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_epoch(&self) -> Result<AnalysisEpoch, String> {
        let state = self.inner.state.lock().unwrap();
        if !state.accepting {
            return Err(IDENTITY_CANCELLED.into());
        }
        Ok(AnalysisEpoch(state.generation))
    }

    pub fn lease(&self, epoch: AnalysisEpoch) -> Result<AnalysisLease, String> {
        let mut state = self.inner.state.lock().unwrap();
        if !state.accepting || state.generation != epoch.0 {
            return Err(IDENTITY_CANCELLED.into());
        }
        state.active += 1;
        Ok(AnalysisLease {
            inner: Arc::clone(&self.inner),
            guard: AnalysisGuard {
                cancelled: Arc::clone(&state.cancelled),
            },
        })
    }

    /// Atomically lease and spawn work for the epoch captured by its command.
    /// A transition either sees this lease in its drain set or makes the spawn
    /// fail; an old command can never attach work to a newer identity.
    pub fn spawn<F, Fut, T>(&self, epoch: AnalysisEpoch, task: F) -> Result<JoinHandle<T>, String>
    where
        F: FnOnce(AnalysisGuard) -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let lease = self.lease(epoch)?;
        let guard = lease.guard();
        Ok(tauri::async_runtime::spawn(async move {
            let _lease = lease;
            task(guard).await
        }))
    }

    /// Cancel the current generation and wait until its complete task tree has
    /// dropped every lease. Dropping the returned barrier admits a fresh
    /// generation for the newly installed identity (or the restored one).
    pub async fn suspend_for_identity_switch(&self) -> Result<AnalysisIdentityBarrier<'_>, String> {
        {
            let mut state = self.inner.state.lock().unwrap();
            if !state.accepting {
                return Err(
                    "Audio analysis is already suspended for an identity transition".into(),
                );
            }
            let next_generation = state
                .generation
                .checked_add(1)
                .ok_or_else(|| "Audio-analysis identity generation overflow".to_string())?;
            state.accepting = false;
            state.cancelled.store(true, Ordering::SeqCst);
            state.generation = next_generation;
        }

        loop {
            let notified = self.inner.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.inner.state.lock().unwrap().active == 0 {
                break;
            }
            notified.await;
        }

        Ok(AnalysisIdentityBarrier { group: self })
    }
}

pub struct AnalysisLease {
    inner: Arc<TaskGroupInner>,
    guard: AnalysisGuard,
}

impl AnalysisLease {
    pub fn guard(&self) -> AnalysisGuard {
        self.guard.clone()
    }
}

impl Drop for AnalysisLease {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock().unwrap();
        debug_assert!(state.active > 0);
        state.active -= 1;
        let drained = state.active == 0;
        drop(state);
        if drained {
            self.inner.drained.notify_waiters();
        }
    }
}

pub struct AnalysisIdentityBarrier<'a> {
    group: &'a AnalysisTaskGroup,
}

impl Drop for AnalysisIdentityBarrier<'_> {
    fn drop(&mut self) {
        let mut state = self.group.inner.state.lock().unwrap();
        debug_assert!(!state.accepting);
        debug_assert_eq!(state.active, 0);
        state.cancelled = Arc::new(AtomicBool::new(false));
        state.accepting = true;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn transition_cancels_drains_and_rejects_stale_epoch_spawns() {
        let group = AnalysisTaskGroup::new();
        let old_epoch = group.current_epoch().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let running = group
            .spawn(old_epoch, move |guard| async move {
                started_tx.send(()).unwrap();
                while !guard.is_cancelled() {
                    tokio::task::yield_now().await;
                }
                release_rx.await.unwrap();
                guard.checkpoint().unwrap_err()
            })
            .unwrap();
        started_rx.await.unwrap();

        let switching_group = group.clone();
        let transition = tokio::spawn(async move {
            let barrier = switching_group.suspend_for_identity_switch().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(barrier);
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            while group.current_epoch().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(group.spawn(old_epoch, |_| async {}).is_err());
        assert!(!transition.is_finished(), "active work must drain first");

        release_tx.send(()).unwrap();
        assert!(running.await.unwrap().contains("identity transition"));
        transition.await.unwrap();

        let new_epoch = group.current_epoch().unwrap();
        assert_ne!(new_epoch, old_epoch);
        assert!(group.spawn(old_epoch, |_| async {}).is_err());
        group.spawn(new_epoch, |_| async {}).unwrap().await.unwrap();
    }
}
