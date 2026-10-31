//! 扩展无法加载或命令无法执行时上报给宿主的错误类型。
//!
//! 每个变体都是 `Clone`：错误消息经 channel 从扩展线程传回宿主，
//! 由宿主上报（有时上报两处：Console + launcher）。

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JsError {
    /// 扩展已注册但从未加载（还没有运行时）。
    #[error("扩展 {0} 尚未加载")]
    NotLoaded(String),

    /// store 不认识这个扩展 bundle。
    #[error("扩展未注册：{0}")]
    UnknownExtension(String),

    /// 命令 id 没有在 `extension.json` 里声明。
    #[error("命令不存在：{0}")]
    UnknownCommand(String),

    /// 三种回退方式（具名导出 / default 对象 / globalThis.plux.commands）
    /// 都没找到命令对应的 JS 函数。
    #[error(
        "扩展没有实现命令 `{0}`（具名导出 / default 对象 / globalThis.plux.commands 均未找到）"
    )]
    MissingHandler(String),

    /// 命令超出时限，被强制中断。
    #[error("命令执行超时（{} ms）", .0.as_millis())]
    Timeout(Duration),

    /// 用户主动取消了这次运行。
    #[error("命令已取消")]
    Cancelled,

    /// `exit(code)` 用抛出哨兵异常的方式结束命令：这是正常终止，不是扩展 bug。
    /// 哨兵格式由宿主解析（见 [`crate::env::JsEnv::classify_error`]），crate 只承载结果。
    #[error("退出码 {0}")]
    Exit(i32),

    /// JS 抛了异常，或模块求值失败。
    #[error("{0}")]
    Script(String),

    /// 扩展发现 / 校验失败。
    #[error("{0}")]
    Invalid(String),

    /// 扩展线程已退出，或从未启动。
    #[error("扩展运行时不可用：{0}")]
    Runtime(String),
}

impl JsError {
    /// 这个错误是正常的控制流结果（取消 / 超时），而不是扩展的 bug。
    /// 宿主据此选择日志级别。
    pub fn is_control_flow(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Cancelled | Self::Exit(_))
    }
}
