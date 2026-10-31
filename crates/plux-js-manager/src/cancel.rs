//! 宿主 UI 与运行中的扩展命令共享的协作式取消 token。
//!
//! 两套相互独立的机制盯着它：QuickJS 的 interrupt handler 抢占长跑的 JS；
//! 宿主侧在等扩展线程回复时轮询它（此时没有 JS 在执行，handler 不会触发）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// [`CancelToken::wait`] 重新检查标志位的间隔。
const POLL: Duration = Duration::from_millis(50);

/// 协作式取消 token：宿主与扩展各持一份克隆（共享底层标志位），
/// 宿主 `cancel`，扩展侧 `wait`/`is_cancelled` 自查。
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// 创建一个未取消状态的 token。
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求中止。幂等，可在任意线程调用。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// 是否已经请求过中止。
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// 挂起等待，直到有人请求过中止。
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
