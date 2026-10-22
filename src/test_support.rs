//! Shared scaffolding for unit tests. Compiled only under `cargo test`.

use std::path::PathBuf;

/// A temporary directory that is removed when dropped.
///
/// The name carries the caller's tag plus the process id, so tests running in
/// parallel never collide.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("bolt-test-{tag}-{}", std::process::id()));
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

/// LSP position constructor.
pub fn position(line: u32, character: u32) -> lsp_types::Position {
    lsp_types::Position { line, character }
}
