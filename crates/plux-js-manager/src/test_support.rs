//! 测试用临时目录（仅 `cfg(test)` 编译）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A temporary directory that is removed when dropped.
///
/// 名字带 caller tag + 进程 id + 自增序号：同一个 tag 被多个测试复用时（并行跑）
/// 也不会互相踩目录。
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("plux-js-test-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// Write `content` to `<dir>/<rel>`, creating parent directories.
    pub fn write(&self, rel: &str, content: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
