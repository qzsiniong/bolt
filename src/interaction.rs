//! Cross-thread bridge between a running script and the app UI.
//!
//! The script runs on a worker thread while everything it needs from the user
//! (progress window, confirm dialog, path picker, focus standby) lives on the
//! GPUI main thread. Requests travel as [`UiRequest`] over a crossbeam channel
//! with a per-request reply channel; progress is a shared snapshot slot the UI
//! polls, because it is overwritten constantly and ordering does not matter.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, bounded};

use crate::runner::CancelToken;

/// Error text thrown when the user aborts (matches clipbeam's
/// `HostError::Cancelled`).
pub const ABORTED: &str = "脚本已中止";

/// Raised when the UI side of the bridge is gone (window closed, run torn down).
const NO_UI: &str = "交互不可用：UI 未就绪";

/// How long a synchronous host call waits before re-checking the cancel token.
const SYNC_POLL: Duration = Duration::from_millis(100);

/// How long an async host call yields to the runtime between checks.
const ASYNC_POLL: Duration = Duration::from_millis(50);

/// What the user is picking in `pickPath`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    File,
    Dir,
}

/// How the user answered `confirm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmChoice {
    Yes,
    No,
    Abort,
}

/// A request from the script that only the UI can answer.
pub enum UiRequest {
    Confirm {
        message: String,
        reply: Sender<ConfirmChoice>,
    },
    PickPath {
        prompt: String,
        kind: PickKind,
        reply: Sender<Option<PathBuf>>,
    },
    RequestFocus {
        hint: String,
        reply: Sender<()>,
    },
}

/// Progress reported by a long-running host call (`typeStr` / `scanQr`).
///
/// `total == 0` means "indeterminate" — the UI renders an animation instead of
/// a percentage.
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressSnapshot {
    pub label: String,
    pub current: u64,
    pub total: u64,
    /// Milliseconds since the operation started. Used by the UI to render
    /// elapsed / remaining time estimates.
    pub elapsed_ms: u64,
}

/// Slot the script writes progress into and the UI reads from. `None` means
/// nothing is in flight.
pub type ProgressSlot = Arc<parking_lot::Mutex<Option<ProgressSnapshot>>>;

/// Script-side handle. Cheap to clone; every clone talks to the same UI.
#[derive(Clone)]
pub struct InteractionBridge {
    tx: Sender<UiRequest>,
    progress: ProgressSlot,
    cancel: CancelToken,
    focus_confirmed: Arc<AtomicBool>,
}

impl InteractionBridge {
    /// Create a bridge plus the receiver the UI must drain.
    pub fn new(cancel: CancelToken) -> (Self, Receiver<UiRequest>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (
            Self {
                tx,
                progress: Arc::new(parking_lot::Mutex::new(None)),
                cancel,
                focus_confirmed: Arc::new(AtomicBool::new(false)),
            },
            rx,
        )
    }

    /// Shared progress slot, handed to the UI side.
    pub fn progress_slot(&self) -> ProgressSlot {
        self.progress.clone()
    }

    pub fn set_progress(&self, snapshot: Option<ProgressSnapshot>) {
        *self.progress.lock() = snapshot;
    }

    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Mark that the user has chosen an input target for this run. Called by the
    /// UI when a standby window is released or dismissed.
    pub fn confirm_focus(&self) {
        self.focus_confirmed.store(true, Ordering::SeqCst);
    }

    /// Whether focus was already confirmed for this run; marks it confirmed.
    pub fn take_focus_confirmation(&self) -> bool {
        self.focus_confirmed.swap(true, Ordering::SeqCst)
    }

    /// `confirm(message)`: yes → `true`, no → `false`, abort → `Err(ABORTED)`.
    pub async fn confirm(&self, message: String) -> Result<bool, String> {
        let (tx, rx) = bounded(1);
        self.send_checked(UiRequest::Confirm { message, reply: tx })?;
        match self.wait_reply_async(&rx).await? {
            ConfirmChoice::Yes => Ok(true),
            ConfirmChoice::No => Ok(false),
            ConfirmChoice::Abort => {
                self.cancel.cancel();
                Err(ABORTED.to_string())
            }
        }
    }

    /// `pickPath(prompt, kind)`: the chosen absolute path, or `None` when the
    /// user cancelled.
    pub async fn pick_path(
        &self,
        prompt: String,
        kind: PickKind,
    ) -> Result<Option<PathBuf>, String> {
        let (tx, rx) = bounded(1);
        self.send_checked(UiRequest::PickPath {
            prompt,
            kind,
            reply: tx,
        })?;
        self.wait_reply_async(&rx).await
    }

    /// `requestFocus(hint)`: returns once the user has put focus on the target
    /// window (the standby window loses activation).
    pub fn request_focus(&self, hint: String) -> Result<(), String> {
        let (tx, rx) = bounded(1);
        self.send_checked(UiRequest::RequestFocus { hint, reply: tx })?;
        self.wait_reply(&rx)
    }

    fn send_checked(&self, request: UiRequest) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            return Err(ABORTED.to_string());
        }
        self.tx.send(request).map_err(|_| NO_UI.to_string())
    }

    /// Block until a reply arrives, re-checking the cancel token between polls.
    /// Used by the synchronous host calls (`requestFocus`, `typeStr`).
    fn wait_reply<T>(&self, rx: &Receiver<T>) -> Result<T, String> {
        loop {
            match rx.recv_timeout(SYNC_POLL) {
                Ok(reply) => return Ok(reply),
                Err(RecvTimeoutError::Timeout) => {
                    if self.cancel.is_cancelled() {
                        return Err(ABORTED.to_string());
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("交互通道已断开".to_string());
                }
            }
        }
    }

    /// Async counterpart of [`Self::wait_reply`]: sleeps on the tokio timer so
    /// the runtime keeps driving the cancel race in the runner.
    async fn wait_reply_async<T>(&self, rx: &Receiver<T>) -> Result<T, String> {
        loop {
            match rx.try_recv() {
                Ok(reply) => return Ok(reply),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    return Err("交互通道已断开".to_string());
                }
            }
            if self.cancel.is_cancelled() {
                return Err(ABORTED.to_string());
            }
            tokio::time::sleep(ASYNC_POLL).await;
        }
    }
}
