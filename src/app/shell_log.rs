//! `tracing` → Console 面板桥。
//!
//! gpui-shell 通过 `tracing` 上报 UI 扩展脚本的 `console` 输出、加载失败与热
//! 重载结果；没有 subscriber 时这些事件会被**静默丢弃**（脚本作者看不到任何
//! 报错）。这里把 target 以 `gpui_shell` 开头的事件转发进应用的 Console 通道，
//! 其余 target（gpui / gpui-base / 依赖库）忽略，避免刷屏。
//!
//! 实现照搬上游 `gpui-shell` 自带 CLI 的极简 `Subscriber`（不引 tracing-subscriber）。

use std::fmt::{self, Write as _};

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Metadata, Subscriber, span};

use crate::console_channel::{ConsoleChannel, ConsoleLevel};

/// UI 扩展脚本 `console.*` 的 target（gpui-shell 内部常量）。
const SCRIPT_TARGET: &str = "gpui_shell::script";

/// 只转发这个前缀下的事件。
const TARGET_PREFIX: &str = "gpui_shell";

/// 安装全局 subscriber。重复安装（已有 subscriber）时保持既有实现。
pub(crate) fn install(console: ConsoleChannel) {
    let max_level = if cfg!(debug_assertions) {
        Level::DEBUG
    } else {
        Level::INFO
    };
    let _ = tracing::subscriber::set_global_default(ConsoleSubscriber { console, max_level });
}

struct ConsoleSubscriber {
    console: ConsoleChannel,
    max_level: Level,
}

impl Subscriber for ConsoleSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= self.max_level
    }

    /// 让 `tracing` 在调用点直接跳过被禁用的事件，而不是每次询问。
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::from_level(self.max_level))
    }

    /// 只打印事件，不记录 span：所有 span 共用一个 id。
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        let metadata = event.metadata();
        if let Some((level, text)) = map_event(*metadata.level(), metadata.target(), &fields.0) {
            self.console.send(level, text);
        }
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

/// 事件字段拼成一行：`message` 是事件本身的文本，裸输出；其余字段带名字。
#[derive(Default)]
struct EventFields(String);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = write!(self.0, "{}", format_field(field.name(), value));
    }
}

/// 事件里的单个字段：`message` 裸输出，其余字段带名字。
fn format_field(name: &str, value: &dyn fmt::Debug) -> String {
    if name == "message" {
        format!("{value:?}")
    } else {
        format!("{name}={value:?}")
    }
}

/// tracing 级别 + target + 文本 → Console 级别与显示文本。
///
/// 扩展脚本的 `console.log/info/debug` 走 INFO（gpui-shell 的选择），这里映射
/// 成 `Log`，与普通脚本的 `console.log` 观感一致；warn/error 一一对应。
/// 非 gpui-shell 的事件返回 `None`，调用方丢弃。
fn map_event(level: Level, target: &str, fields: &str) -> Option<(ConsoleLevel, String)> {
    if !target.starts_with(TARGET_PREFIX) {
        return None;
    }
    let (level, text) = if target == SCRIPT_TARGET {
        let level = match level {
            Level::ERROR => ConsoleLevel::Error,
            Level::WARN => ConsoleLevel::Warn,
            _ => ConsoleLevel::Log,
        };
        (level, fields.to_string())
    } else {
        let level = match level {
            Level::ERROR => ConsoleLevel::Error,
            Level::WARN => ConsoleLevel::Warn,
            _ => ConsoleLevel::Info,
        };
        (level, format!("{target}: {fields}"))
    };
    Some((level, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_info_maps_to_log() {
        assert_eq!(
            map_event(Level::INFO, SCRIPT_TARGET, "hello"),
            Some((ConsoleLevel::Log, "hello".to_string()))
        );
        assert_eq!(
            map_event(Level::DEBUG, SCRIPT_TARGET, "debug line"),
            Some((ConsoleLevel::Log, "debug line".to_string()))
        );
    }

    #[test]
    fn script_warn_and_error_keep_their_level() {
        assert_eq!(
            map_event(Level::WARN, SCRIPT_TARGET, "careful"),
            Some((ConsoleLevel::Warn, "careful".to_string()))
        );
        assert_eq!(
            map_event(Level::ERROR, SCRIPT_TARGET, "boom"),
            Some((ConsoleLevel::Error, "boom".to_string()))
        );
    }

    #[test]
    fn host_events_keep_target_and_use_info() {
        assert_eq!(
            map_event(Level::INFO, "gpui_shell::load", "reloaded"),
            Some((ConsoleLevel::Info, "gpui_shell::load: reloaded".to_string()))
        );
    }

    #[test]
    fn other_targets_are_dropped() {
        assert_eq!(map_event(Level::INFO, "gpui::window", "paint"), None);
        assert_eq!(map_event(Level::ERROR, "llrt", "nope"), None);
    }

    #[test]
    fn fields_render_message_bare_and_named_values_with_names() {
        assert_eq!(format_field("message", &format_args!("text")), "text");
        assert_eq!(format_field("code", &7), "code=7");
    }
}
