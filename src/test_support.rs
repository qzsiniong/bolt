//! Shared scaffolding for unit tests. Compiled only under `cargo test`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::console_channel::ConsoleChannel;
use crate::host::{HostRegistry, modules};
use crate::model::script::Language;
use crate::runtime::interaction::InteractionBridge;
use crate::runtime::runner::{CancelToken, RunEnv, run_script};

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

/// Run a JS snippet through the real runner (all built-in host modules
/// registered) and return the console lines. Host-layer tests use it to
/// exercise the public `import { … } from 'bolt'` path end-to-end.
pub fn run_js(code: &str, scripts_dir: &Path) -> Vec<String> {
    let mut registry = HostRegistry::new();
    modules::register_all(&mut registry);
    run_with_registry(code, scripts_dir, Arc::new(registry))
}

/// Same as [`run_js`] with a custom registry (e.g. to test loader precedence).
pub fn run_with_registry(
    code: &str,
    scripts_dir: &Path,
    registry: Arc<HostRegistry>,
) -> Vec<String> {
    let channel = ConsoleChannel::new();
    let cancel = CancelToken::new();
    let env = RunEnv {
        scripts_dir: scripts_dir.to_path_buf(),
        registry,
        console: channel.clone(),
        cancel: cancel.clone(),
        interaction: InteractionBridge::new(cancel).0,
        plugins: None,
    };
    run_script(code, Language::JavaScript, "test.js", env);
    channel
        .history()
        .entries()
        .into_iter()
        .map(|e| e.text)
        .collect()
}
