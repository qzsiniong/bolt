//! 运行中的脚本与 UI 之间的跨线程桥。
//!
//! 脚本跑在工作线程上，而它需要用户配合的一切（进度窗、确认框、路径选择、
//! 待命焦点窗）都在 GPUI 主线程。请求以 [`UiRequest`] 经 crossbeam 通道发出，
//! 每个请求自带一条回复通道；进度则是一块共享快照槽，由 UI 轮询读取——它被
//! 高频覆盖，先后顺序无所谓，不值得为每条进度单独发消息。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, bounded};

use super::runner::CancelToken;

/// 用户中止时抛出的错误文本（与 clipbeam 的 `HostError::Cancelled` 对齐）。
pub const ABORTED: &str = "脚本已中止";

/// 桥的 UI 侧已消失（窗口关闭、运行被拆除）时的提示。
const NO_UI: &str = "交互不可用：UI 未就绪";

/// 同步宿主调用等待多久后重新检查一次取消令牌。
const SYNC_POLL: Duration = Duration::from_millis(100);

/// 异步宿主调用在两次检查之间让出运行时的间隔。
const ASYNC_POLL: Duration = Duration::from_millis(50);

/// `pickPath` 让用户挑选的对象。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    File,
    Dir,
}

/// `confirm` 的用户答复。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmChoice {
    Yes,
    No,
    Abort,
}

/// 脚本发来的、只有 UI 能回答的请求。
pub enum UiRequest {
    /// 弹确认框，答复经 `reply` 回传。
    Confirm {
        message: String,
        reply: Sender<ConfirmChoice>,
    },
    /// 打开原生路径选择器；用户取消时回传 `None`。
    PickPath {
        prompt: String,
        kind: PickKind,
        reply: Sender<Option<PathBuf>>,
    },
    /// 进入焦点待命，等用户把焦点交给目标窗口。
    RequestFocus { hint: String, reply: Sender<()> },
}

/// 长时间宿主调用（`typeStr` / `scanQr`）回报的进度。
///
/// `total == 0` 表示「无法确定总量」——此时 UI 画动画而不是百分比。
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressSnapshot {
    pub label: String,
    pub current: u64,
    pub total: u64,
    /// 操作开始至今的毫秒数。UI 用它估算已用时间与剩余时间。
    pub elapsed_ms: u64,
}

/// 脚本写、UI 读的进度槽。`None` 表示当前没有进行中的操作。
pub type ProgressSlot = Arc<parking_lot::Mutex<Option<ProgressSnapshot>>>;

/// 脚本侧句柄。克隆代价很低，所有克隆都连到同一个 UI。
#[derive(Clone)]
pub struct InteractionBridge {
    tx: Sender<UiRequest>,
    progress: ProgressSlot,
    cancel: CancelToken,
    focus_confirmed: Arc<AtomicBool>,
}

impl InteractionBridge {
    /// 创建一座桥，并返回 UI 侧必须持续排空的接收端。
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

    /// 共享的进度槽，交给 UI 侧读取。
    pub fn progress_slot(&self) -> ProgressSlot {
        self.progress.clone()
    }

    /// 覆盖当前进度快照，传 `None` 表示操作已结束。
    pub fn set_progress(&self, snapshot: Option<ProgressSnapshot>) {
        *self.progress.lock() = snapshot;
    }

    /// 本轮运行的取消令牌副本。
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// 标记本轮已确定输入目标。UI 在待命窗被释放或关闭时调用。
    pub fn confirm_focus(&self) {
        self.focus_confirmed.store(true, Ordering::SeqCst);
    }

    /// 查询本轮焦点是否已确认，同时把它置为已确认（一次性读取）。
    pub fn take_focus_confirmation(&self) -> bool {
        self.focus_confirmed.swap(true, Ordering::SeqCst)
    }

    /// `confirm(message)`：是 → `true`，否 → `false`，中止 → `Err(ABORTED)`。
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

    /// `pickPath(prompt, kind)`：返回用户选中的绝对路径；用户取消时为 `None`。
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

    /// `requestFocus(hint)`：等用户把焦点放到目标窗口（待命窗失去激活）后返回。
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

    /// 阻塞等待回复，每次轮询间隙重新检查取消令牌。
    /// 供同步宿主调用（`requestFocus`、`typeStr`）使用。
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

    /// [`Self::wait_reply`] 的异步版本：在 tokio 定时器上让出，好让运行时继续
    /// 推动运行器里的取消竞争。
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
