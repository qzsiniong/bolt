use crossbeam_channel::{Receiver, Sender, unbounded};
use std::time::{SystemTime, UNIX_EPOCH};

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
#[derive(Clone)]
pub struct ConsoleChannel {
    tx: Sender<ConsoleEntry>,
}

impl ConsoleChannel {
    /// Create a channel and return the sender handle plus the receiver the
    /// panel should drain.
    pub fn new() -> (Self, Receiver<ConsoleEntry>) {
        let (tx, rx) = unbounded();
        (Self { tx }, rx)
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
        let _ = self.tx.send(ConsoleEntry {
            level,
            text,
            timestamp: now_ms(),
        });
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
