//! 测试用临时目录（仅 `cfg(test)` 编译）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// drop 时自动删除的临时目录。
///
/// 名字带 caller tag + 进程 id + 自增序号：同一个 tag 被多个测试复用时（并行跑）
/// 也不会互相踩目录。
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("bolt-ext-test-{tag}-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// 把 `content` 写到 `<dir>/<rel>`，父目录不存在则自动创建。
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
