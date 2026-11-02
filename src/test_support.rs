//! 单元测试共用的脚手架。只在 `cargo test` 下编译。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::host::{HostRegistry, modules};
use crate::runtime::interaction::InteractionBridge;
use crate::runtime::runner::{CancelToken, RunEnv, run_script};
use bolt_console::ConsoleChannel;
use bolt_model::script::Language;

/// 临时目录，drop 时整棵删掉。
///
/// 目录名带上调用方的 tag 和进程 id，并行跑的测试不会撞车。
pub struct TempDir(pub PathBuf);

impl TempDir {
    /// 建一个干净的同名临时目录（先删旧目录，避免上次残留）。
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("bolt-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// 把 `content` 写到 `<dir>/<rel>`，父目录不存在就建出来。
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

/// 用真实 runner（注册全部内建 host 模块）跑一段 JS，返回 Console 里的行。
/// host 层测试靠它端到端走一遍公开的 `import { … } from 'bolt'` 路径。
pub fn run_js(code: &str, scripts_dir: &Path) -> Vec<String> {
    let mut registry = HostRegistry::new();
    modules::register_all(&mut registry);
    run_with_registry(code, scripts_dir, Arc::new(registry))
}

/// 同 [`run_js`]，但用自带的 registry（例如测加载器优先级）。
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
        extensions: None,
    };
    run_script(code, Language::JavaScript, "test.js", env);
    channel
        .history()
        .entries()
        .into_iter()
        .map(|e| e.text)
        .collect()
}
