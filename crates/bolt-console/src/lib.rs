//! Console 输出通道：进程级共享缓冲 + 各读者自持游标取增量。
//!
//! 写入方是 [`ConsoleChannel`]，读取方通过 [`ConsoleChannel::history`] 拿到同一个
//! [`ConsoleLog`]，因此 Console 面板和独立 Console 窗口能同时看同一份输出；Error
//! 级条目还额外滚动落盘，打包成 .app 后 stderr 不可见，落盘是唯一线索。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Local};

/// Console 缓冲区容量。所有读者（脚本管理器的 Console 面板、独立 Console 窗口）
/// 共享这一份缓冲；面板自身也只保留这么多条（`MAX_ENTRIES`），两边对齐。
const CONSOLE_CAP: usize = 1000;

/// Error 级条目落盘 `bolt.log` 时保留的最大行数（滚动裁剪）。
const MAX_LOG_LINES: usize = 500;

/// 一行 Console 输出的级别，决定它在 Console 面板里的颜色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
}

impl ConsoleLevel {
    /// 全部级别，按严重程度从低到高。
    pub const ALL: [Self; 4] = [Self::Log, Self::Info, Self::Warn, Self::Error];

    /// 级别名（也写进 `bolt.log`）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Log => "Log",
            Self::Info => "Info",
            Self::Warn => "Warn",
            Self::Error => "Error",
        }
    }

    /// 终端镜像用的行首标记，Log 不带标记。
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Log => "",
            Self::Info => "[i] ",
            Self::Warn => "[!] ",
            Self::Error => "[x] ",
        }
    }
}

/// 一条 Console 输出。
#[derive(Clone, Debug)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub text: String,
    /// Unix 纪元毫秒时间戳。
    pub timestamp: i64,
}

/// 进程级输出缓冲：写入方是 [`ConsoleChannel`]，读者各自持一个游标取增量。
///
/// 早先这里是「有界 channel + 唯一的 `Receiver`」——面板一旦建立就独占消费，
/// 第二个读者（独立 Console 窗口）拿不到任何历史。改成共享缓冲后，谁开谁关
/// 只影响自己的游标，面板与窗口可以同时显示同一份输出。
#[derive(Clone, Default)]
pub struct ConsoleLog {
    inner: Arc<Mutex<LogInner>>,
}

#[derive(Default)]
struct LogInner {
    entries: VecDeque<ConsoleEntry>,
    /// 累计写入条数；`entries` 里最早一条的编号是 `revision - entries.len()`。
    revision: u64,
}

impl ConsoleLog {
    /// 新建一个空的共享缓冲。
    pub fn new() -> Self {
        Self::default()
    }

    /// 累计写入条数，可直接当游标用。
    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    /// `cursor` 之后的新条目 + 新游标。
    ///
    /// `cursor` 指向的条目已被容量裁掉时，从现存最早一条开始给：宁可多给几条，
    /// 不能漏（窗口本来就只保留最近 1000 条，多给也只多到这里）。
    pub fn since(&self, cursor: u64) -> (Vec<ConsoleEntry>, u64) {
        let inner = self.lock();
        let revision = inner.revision;
        let oldest = revision.saturating_sub(inner.entries.len() as u64);
        let skip = cursor.max(oldest) - oldest;
        let entries = inner.entries.iter().skip(skip as usize).cloned().collect();
        (entries, revision)
    }

    /// 全部现存条目（调试/测试用）。
    pub fn entries(&self) -> Vec<ConsoleEntry> {
        self.lock().entries.iter().cloned().collect()
    }

    fn push(&self, entry: ConsoleEntry) {
        let mut inner = self.lock();
        inner.entries.push_back(entry);
        inner.revision += 1;
        while inner.entries.len() > CONSOLE_CAP {
            inner.entries.pop_front();
        }
    }

    /// 锁中毒（某个持有者 panic）时继续用里面的数据：Console 输出不该让整个
    /// 应用跟着炸。
    fn lock(&self) -> MutexGuard<'_, LogInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 写入方（runner 及任何 Rust 侧代码）用来往 Console 推行的可克隆句柄。读取方
/// 通过 [`Self::history`] 拿到共享的 [`ConsoleLog`]。
///
/// 缓冲有上限（`CONSOLE_CAP`）：长时间没人读而输出持续到来时，挤出最旧的条目
/// 腾地方，因此它始终保留最近的行，而不是无限增长。
#[derive(Clone)]
pub struct ConsoleChannel {
    log: ConsoleLog,
    /// `Some` 时 Error 级条目同步落盘（打包成 .app 后 stderr 不可见，
    /// 落盘是启动失败唯一的出路）。
    error_log: Option<Arc<ErrorLog>>,
    /// 调试构建下把每条输出同时打到终端，见 [`mirror_to_terminal`]。
    mirror: bool,
}

impl Default for ConsoleChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleChannel {
    /// 建一个既不落盘也不镜像到终端的通道。测试与嵌入场景用；生产走
    /// [`new_with_error_log`]。
    pub fn new() -> Self {
        Self::new_inner(None, false)
    }

    /// 同 [`new`]，但 `Error` 级条目额外追加到 `log_path`（滚动保留最后
    /// [`MAX_LOG_LINES`] 行），调试构建下每行还镜像到终端。
    pub fn new_with_error_log(log_path: PathBuf) -> Self {
        Self::new_inner(
            Some(Arc::new(ErrorLog {
                path: log_path,
                lock: Mutex::new(()),
            })),
            terminal_mirror_enabled(),
        )
    }

    #[cfg(test)]
    fn new_quiet_with_error_log(log_path: PathBuf) -> Self {
        Self::new_inner(
            Some(Arc::new(ErrorLog {
                path: log_path,
                lock: Mutex::new(()),
            })),
            false,
        )
    }

    fn new_inner(error_log: Option<Arc<ErrorLog>>, mirror: bool) -> Self {
        Self {
            log: ConsoleLog::new(),
            error_log,
            mirror,
        }
    }

    /// 共享缓冲句柄：每个读者各自持游标，互不影响。
    pub fn history(&self) -> ConsoleLog {
        self.log.clone()
    }

    /// 写一条 Info 级输出。
    pub fn info(&self, text: impl Into<String>) {
        self.send(ConsoleLevel::Info, text.into());
    }
    /// 写一条 Warn 级输出。
    pub fn warn(&self, text: impl Into<String>) {
        self.send(ConsoleLevel::Warn, text.into());
    }
    /// 写一条 Error 级输出；配置了落盘时会同步写进 `bolt.log`。
    pub fn error(&self, text: impl Into<String>) {
        self.send(ConsoleLevel::Error, text.into());
    }

    /// 按级别写一条输出：先落盘（Error 且配置了日志时），再进共享缓冲。
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
        self.log.push(entry.clone());
        if self.mirror {
            mirror_to_terminal(&entry);
        }
    }
}

/// 输出该走哪一路标准流。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalStream {
    Out,
    Err,
}

/// Log/Info 走 stdout，Warn/Error 走 stderr——这样 `cargo run 2>/dev/null` 仍能
/// 看到正常输出，管道里也还能按流转发。
fn terminal_stream(level: ConsoleLevel) -> TerminalStream {
    match level {
        ConsoleLevel::Log | ConsoleLevel::Info => TerminalStream::Out,
        ConsoleLevel::Warn | ConsoleLevel::Error => TerminalStream::Err,
    }
}

/// 调试构建下把一条输出同时打到终端，`cargo run` 的窗口里就能直接看到，
/// 不必再去点开 Console 窗口。
///
/// 发布构建默认静默（打包成 .app 后根本没有终端），需要时用环境变量
/// `BOLT_CONSOLE_TERMINAL=1` 打开、`=0` 关掉。
fn mirror_to_terminal(entry: &ConsoleEntry) {
    use std::io::Write as _;

    let line = format!("{}{}", entry.level.prefix(), entry.text);
    match terminal_stream(entry.level) {
        TerminalStream::Out => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{line}");
            // 逐行 flush：输出常常是崩溃/退出前的最后一条，攒在缓冲里就丢了。
            let _ = out.flush();
        }
        TerminalStream::Err => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{line}");
            let _ = err.flush();
        }
    }
}

/// 终端镜像默认值：调试构建开，发布构建关；`BOLT_CONSOLE_TERMINAL` 可覆盖。
fn terminal_mirror_enabled() -> bool {
    match std::env::var("BOLT_CONSOLE_TERMINAL").as_deref() {
        Ok("0") | Ok("off") | Ok("no") => false,
        Ok("1") | Ok("on") | Ok("yes") => true,
        _ => cfg!(debug_assertions),
    }
}

/// 滚动错误日志：追加 `Error` 条目，只保留最后 [`MAX_LOG_LINES`] 行。所有 IO
/// 都尽力而为——日志文件坏了，也绝不能连带把 Console 通道拖垮。
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
        let channel = ConsoleChannel::new();
        for i in 0..(CONSOLE_CAP + 50) {
            channel.info(format!("line-{i}"));
        }
        let texts: Vec<String> = channel
            .history()
            .entries()
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(texts.len(), CONSOLE_CAP, "buffer must stay bounded");
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
    fn since_returns_only_the_new_entries() {
        let channel = ConsoleChannel::new();
        let log = channel.history();

        assert!(log.since(0).0.is_empty(), "empty log has no entries");
        assert_eq!(log.since(0).1, 0);

        channel.info("one");
        channel.warn("two");
        let (entries, cursor) = log.since(0);
        assert_eq!(
            entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(cursor, 2);

        channel.error("three");
        let (entries, cursor) = log.since(cursor);
        assert_eq!(
            entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["three"]
        );
        assert_eq!(cursor, 3);
        // 游标到顶后再问一次：什么都没有，但游标不回退。
        let (nothing, same) = log.since(cursor);
        assert!(nothing.is_empty());
        assert_eq!(same, 3);
    }

    #[test]
    fn since_after_trimming_restarts_from_the_oldest_survivor() {
        let channel = ConsoleChannel::new();
        let log = channel.history();
        for i in 0..(CONSOLE_CAP + 10) {
            channel.info(format!("line-{i}"));
        }
        // 0 号早被裁掉：从现存最早一条开始给，不会漏给"空"。
        let (entries, cursor) = log.since(0);
        assert_eq!(entries.len(), CONSOLE_CAP);
        assert_eq!(entries[0].text, "line-10");
        assert_eq!(cursor, CONSOLE_CAP as u64 + 10);
    }

    #[test]
    fn levels_split_between_stdout_and_stderr() {
        assert_eq!(terminal_stream(ConsoleLevel::Log), TerminalStream::Out);
        assert_eq!(terminal_stream(ConsoleLevel::Info), TerminalStream::Out);
        assert_eq!(terminal_stream(ConsoleLevel::Warn), TerminalStream::Err);
        assert_eq!(terminal_stream(ConsoleLevel::Error), TerminalStream::Err);
    }

    #[test]
    fn error_entries_are_written_to_the_log_and_rolled() {
        let tmp = tempfile_dir();
        let log = tmp.join("nested/bolt.log");
        let channel = ConsoleChannel::new_quiet_with_error_log(log.clone());

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
