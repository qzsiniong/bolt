use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use crossbeam_channel::{Receiver, Sender, bounded};

/// Console 通道容量。面板自身也只保留这么多条（`MAX_ENTRIES`），两边对齐：
/// 面板长时间不打开时，通道缓存的是"最近 1000 条"而不是无限累积。
const CONSOLE_CAP: usize = 1000;

/// Error 级条目落盘 `bolt.log` 时保留的最大行数（滚动裁剪）。
const MAX_LOG_LINES: usize = 500;

/// Severity of a console line. Drives the color in the Console panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
}

impl ConsoleLevel {
    pub const ALL: [Self; 4] = [Self::Log, Self::Info, Self::Warn, Self::Error];

    pub fn label(self) -> &'static str {
        match self {
            Self::Log => "Log",
            Self::Info => "Info",
            Self::Warn => "Warn",
            Self::Error => "Error",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Self::Log => "",
            Self::Info => "[i] ",
            Self::Warn => "[!] ",
            Self::Error => "[x] ",
        }
    }
}

/// A single console output line.
#[derive(Clone, Debug)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub text: String,
    /// Unix epoch milliseconds.
    pub timestamp: i64,
}

/// Cloneable handle used by the runner (and any Rust side) to push lines into
/// the Console panel. The matching `Receiver` lives in [`ConsolePanel`].
///
/// The channel is bounded (`CONSOLE_CAP`): when the panel has not been opened
/// for a long time and output keeps arriving, the oldest entry is dropped to
/// make room, so the buffer always holds the most recent lines instead of
/// growing without limit.
#[derive(Clone)]
pub struct ConsoleChannel {
    tx: Sender<ConsoleEntry>,
    /// Private receiver clone: the overflow valve. Only the sender side pops
    /// from it, to free a slot when the buffer is full.
    rx: Receiver<ConsoleEntry>,
    /// `Some` 时 Error 级条目同步落盘（打包成 .app 后 stderr 不可见，
    /// Console 面板又只在脚本管理器窗口里，落盘是启动失败唯一的出路）。
    error_log: Option<Arc<ErrorLog>>,
}

impl ConsoleChannel {
    /// Create a channel and return the sender handle plus the receiver the
    /// panel should drain. Errors are NOT written to disk — tests use this;
    /// production goes through [`new_with_error_log`].
    #[cfg(test)]
    pub fn new() -> (Self, Receiver<ConsoleEntry>) {
        Self::new_inner(None)
    }

    /// Same as [`new`], but `Error`-level entries are also appended to
    /// `log_path` (rolling, keeps the last [`MAX_LOG_LINES`] lines).
    pub fn new_with_error_log(log_path: PathBuf) -> (Self, Receiver<ConsoleEntry>) {
        Self::new_inner(Some(Arc::new(ErrorLog {
            path: log_path,
            lock: Mutex::new(()),
        })))
    }

    fn new_inner(error_log: Option<Arc<ErrorLog>>) -> (Self, Receiver<ConsoleEntry>) {
        let (tx, rx) = bounded(CONSOLE_CAP);
        (
            Self {
                tx: tx.clone(),
                rx: rx.clone(),
                error_log,
            },
            rx,
        )
    }

    pub fn info(&self, text: impl Into<String>) {
        self.send(ConsoleLevel::Info, text.into());
    }
    pub fn warn(&self, text: impl Into<String>) {
        self.send(ConsoleLevel::Warn, text.into());
    }
    pub fn error(&self, text: impl Into<String>) {
        self.send(ConsoleLevel::Error, text.into());
    }

    pub fn send(&self, level: ConsoleLevel, text: String) {
        let entry = ConsoleEntry {
            level,
            text,
            timestamp: now_ms(),
        };
        if level == ConsoleLevel::Error
            && let Some(log) = &self.error_log
        {
            log.append(&entry);
        }
        // 满了就丢最旧的：面板只关心最近发生的事。try_send 与消费端并发时
        // 可能竞争失败——丢一条输出无所谓，不能为此阻塞发送方。
        if self.tx.is_full() {
            let _ = self.rx.try_recv();
        }
        let _ = self.tx.try_send(entry);
    }
}

/// Rolling error log: appends `Error` entries, keeps the last
/// [`MAX_LOG_LINES`] lines. All IO is best-effort — a broken log file must
/// never take the console channel down with it.
struct ErrorLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ErrorLog {
    fn append(&self, entry: &ConsoleEntry) {
        let _guard = self.lock.lock();
        let time = DateTime::from_timestamp_millis(entry.timestamp)
            .map(|t| t.with_timezone(&Local).format("%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| format!("ts={}", entry.timestamp));
        // 错误文本可能带换行（如 TS 转换失败的多行消息）；转义掉，保证
        // "一条日志 = 一个物理行"，滚动裁剪才不会把栈截成两半。
        let text = entry.text.replace('\n', "\\n");
        let line = format!("[{time}] [{}] {}", entry.level.label(), text);

        // 追加 + 滚动裁剪：错误条目频率很低，整文件重写完全可接受。
        let mut lines: Vec<String> = std::fs::read_to_string(&self.path)
            .map(|content| content.lines().map(str::to_string).collect())
            .unwrap_or_default();
        lines.push(line);
        if lines.len() > MAX_LOG_LINES {
            let drop_n = lines.len() - MAX_LOG_LINES;
            lines.drain(..drop_n);
        }
        if let Some(parent) = self.path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!("create log dir {}: {e}", parent.display());
            return;
        }
        let _ = std::fs::write(&self.path, lines.join("\n") + "\n");
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn overflow_drops_the_oldest_entry() {
        let (channel, rx) = ConsoleChannel::new();
        for i in 0..(CONSOLE_CAP + 50) {
            channel.info(format!("line-{i}"));
        }
        let texts: Vec<String> = rx.try_iter().map(|e| e.text).collect();
        assert_eq!(texts.len(), CONSOLE_CAP, "channel must stay bounded");
        // 最旧的 50 条被挤出；其余按序保留，最后一条一定在。
        assert!(
            !texts.iter().any(|t| t == "line-0"),
            "oldest must be dropped"
        );
        assert!(
            !texts.iter().any(|t| t == "line-49"),
            "all 50 overflowed entries must be dropped"
        );
        assert!(
            texts.iter().any(|t| t == "line-50"),
            "the first surviving entry must be line-50"
        );
        assert_eq!(texts.last().map(String::as_str), Some("line-1049"));
    }

    #[test]
    fn error_entries_are_written_to_the_log_and_rolled() {
        let tmp = tempfile_dir();
        let log = tmp.join("nested/bolt.log");
        let (channel, _rx) = ConsoleChannel::new_with_error_log(log.clone());

        channel.info("info stays out of the log");
        channel.error("first failure");
        channel.warn("warn stays out of the log");
        channel.error("second failure");

        let content = std::fs::read_to_string(&log).unwrap();
        assert!(content.contains("first failure"), "{content}");
        assert!(content.contains("second failure"), "{content}");
        assert!(!content.contains("info stays"), "{content}");
        assert!(!content.contains("warn stays"), "{content}");

        // 滚动：超过上限后只剩最后 MAX_LOG_LINES 行。
        for i in 0..(MAX_LOG_LINES + 10) {
            channel.error(format!("err-{i}"));
        }
        let content = std::fs::read_to_string(&log).unwrap();
        assert_eq!(content.lines().count(), MAX_LOG_LINES);
        assert!(
            content.contains(&format!("err-{}", MAX_LOG_LINES + 9)),
            "newest error must survive the roll"
        );
        assert!(
            !content.contains("first failure"),
            "oldest lines must be trimmed"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn tempfile_dir() -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("console-channel-{}-{n}", std::process::id()))
    }
}
