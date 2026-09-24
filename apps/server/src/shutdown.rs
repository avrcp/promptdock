use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone, Debug)]
pub struct TaskSupervisor {
    cancellation: CancellationToken,
    tracker: TaskTracker,
    abort_handles: Arc<Mutex<Vec<AbortHandle>>>,
}

impl Default for TaskSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskSupervisor {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            tracker: TaskTracker::new(),
            abort_handles: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let handle = self.tracker.spawn(future);
        if let Ok(mut handles) = self.abort_handles.lock() {
            handles.retain(|handle| !handle.is_finished());
            handles.push(handle.abort_handle());
        }
        handle
    }

    pub fn begin_shutdown(&self) {
        self.cancellation.cancel();
        self.tracker.close();
    }

    pub async fn wait(&self) {
        self.tracker.wait().await;
    }

    pub fn abort_all(&self) {
        if let Ok(mut handles) = self.abort_handles.lock() {
            for handle in handles.drain(..) {
                handle.abort();
            }
        }
    }
}

pub async fn first_signal<A, B, E>(first: A, second: B) -> Result<(), E>
where
    A: Future<Output = Result<(), E>>,
    B: Future<Output = Result<(), E>>,
{
    tokio::select! {
        result = first => result,
        result = second => result,
    }
}

pub async fn os_signal() -> Result<(), SignalError> {
    let ctrl_c = async { tokio::signal::ctrl_c().await.map_err(|_| SignalError) };

    #[cfg(unix)]
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|_| SignalError)?;
        signal.recv().await.ok_or(SignalError)?;
        Ok(())
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<Result<(), SignalError>>();

    first_signal(ctrl_c, terminate).await
}

pub async fn wait_with_deadline<F, T>(deadline: Duration, future: F) -> Result<T, ShutdownTimeout>
where
    F: Future<Output = T>,
{
    tokio::time::timeout(deadline, future)
        .await
        .map_err(|_| ShutdownTimeout)
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
#[error("server shutdown exceeded configured deadline")]
pub struct ShutdownTimeout;

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
#[error("operating system shutdown signal listener failed")]
pub struct SignalError;

#[cfg(test)]
mod tests {
    use std::{future::pending, sync::Arc};

    use tokio::sync::{Notify, oneshot};

    use super::*;

    #[tokio::test]
    async fn either_shutdown_signal_completes_selection() {
        for first in [true, false] {
            let (left_tx, left_rx) = oneshot::channel();
            let (right_tx, right_rx) = oneshot::channel();
            let selected = tokio::spawn(first_signal(
                async { left_rx.await.map_err(|_| ()) },
                async { right_rx.await.map_err(|_| ()) },
            ));
            if first {
                left_tx.send(()).expect("left receiver");
            } else {
                right_tx.send(()).expect("right receiver");
            }
            assert_eq!(selected.await.expect("selection task"), Ok(()));
        }
    }

    #[tokio::test]
    async fn supervisor_cancels_and_joins_cooperative_tasks() {
        let supervisor = TaskSupervisor::new();
        let token = supervisor.cancellation_token();
        let acknowledged = Arc::new(Notify::new());
        let task_ack = Arc::clone(&acknowledged);
        let handle = supervisor.spawn(async move {
            token.cancelled().await;
            task_ack.notify_one();
        });

        supervisor.begin_shutdown();
        acknowledged.notified().await;
        supervisor.wait().await;
        handle.await.expect("cooperative task");
    }

    #[tokio::test]
    async fn critical_task_panic_is_observable_to_its_owner() {
        let supervisor = TaskSupervisor::new();
        let handle = supervisor.spawn(async { panic!("critical worker failed") });
        let error = handle.await.expect_err("panic must produce JoinError");
        assert!(error.is_panic());
        supervisor.begin_shutdown();
        supervisor.wait().await;
    }

    #[tokio::test]
    async fn abort_all_terminates_non_cooperative_tasks() {
        let supervisor = TaskSupervisor::new();
        let handle = supervisor.spawn(std::future::pending::<()>());
        supervisor.begin_shutdown();
        supervisor.abort_all();
        supervisor.wait().await;
        assert!(
            handle
                .await
                .expect_err("task must be aborted")
                .is_cancelled()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_is_enforced_without_wall_clock_sleep() {
        let wait = tokio::spawn(wait_with_deadline(Duration::from_secs(15), pending::<()>()));
        tokio::time::advance(Duration::from_secs(14)).await;
        assert!(!wait.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(wait.await.expect("deadline task"), Err(ShutdownTimeout));
    }
}
