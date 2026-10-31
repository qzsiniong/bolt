//! Cooperative cancellation token shared between the host UI and a running
//! extension command.
//!
//! Two independent mechanisms watch it: QuickJS's interrupt handler preempts
//! long-running JS, and the host side polls it while waiting for the extension
//! thread's reply (where no JS executes, so the handler never fires).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often [`CancelToken::wait`] re-checks the flag.
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request an abort. Idempotent and callable from any thread.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Resolve once an abort has been requested.
    pub async fn wait(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(POLL).await;
        }
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}
