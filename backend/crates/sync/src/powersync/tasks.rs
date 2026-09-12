use std::future::Future;

use tokio::{runtime::Handle, sync::oneshot, task::JoinHandle};

pub(super) struct Task {
    stop: Option<oneshot::Sender<()>>,
    join: JoinHandle<()>,
}

impl Task {
    pub(super) fn spawn(future: impl Future<Output = ()> + Send + 'static) -> Self {
        let runtime = Handle::current();
        let (stop, stopped) = oneshot::channel();
        // SDK SQLite calls block. Each actor needs its own thread so a busy
        // writer cannot prevent the app or the other actor from releasing a lock.
        let join = tokio::task::spawn_blocking(move || {
            runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = stopped => {},
                    _ = future => {},
                }
            });
        });
        Self {
            stop: Some(stop),
            join,
        }
    }

    pub(super) async fn stop(mut self) {
        self.stop.take();
        let _ = (&mut self.join).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::powersync::{
        sdk::schema::{Column, Schema, Table},
        Connections, HttpClient,
    };
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn sdk_lock_wait_does_not_block_the_application_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let connections = Connections::open(&directory.path().join("locks.db"), 1)
            .await
            .unwrap();
        let schema = Schema {
            tables: vec![Table::create(
                "records",
                vec![Column::text("value")],
                |_| {},
            )],
            ..Default::default()
        };
        let database = connections.start(schema, HttpClient::new()).await.unwrap();
        sqlx::query("INSERT INTO records(id,value) VALUES('record','queued')")
            .execute(&database.sql)
            .await
            .unwrap();
        let local = database.sql.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let sync = database.sync.clone();
        let (started, starting) = oneshot::channel();
        let (finished, finishing) = oneshot::channel();
        let task = Task::spawn(async move {
            let upload = sync.next_crud_transaction().await.unwrap().unwrap();
            started.send(()).unwrap();
            let _ = finished.send(upload.complete().await);
        });
        starting.await.unwrap();
        let start = Instant::now();
        // Let the SDK enter its synchronous busy handler while this transaction holds the lock.
        tokio::time::sleep(Duration::from_millis(50)).await;
        local.commit().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), finishing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "SDK lock wait blocked the app runtime"
        );
        task.stop().await;
        database.close().await;
    }

    #[tokio::test]
    async fn dropping_task_cancels_it_even_when_its_future_never_completes() {
        let (dropped, drop_seen) = oneshot::channel::<()>();
        let (started, start_seen) = oneshot::channel();
        let task = Task::spawn(async move {
            let _lifetime = dropped;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        start_seen.await.unwrap();
        drop(task);
        assert!(tokio::time::timeout(Duration::from_secs(2), drop_seen)
            .await
            .unwrap()
            .is_err());
    }
}
