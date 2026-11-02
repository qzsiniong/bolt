//! 未支持平台的兜底实现。
//!
//! 全部返回明确错误而不是静默成功：让用户在 Console / 通知里看到「当前平台不
//! 支持」，而不是点了没反应。

use std::path::{Path, PathBuf};

const HINT: &str = "当前平台尚未支持该操作";

pub(super) fn open_path(_path: &Path) -> Result<(), String> {
    Err(HINT.to_string())
}

pub(super) fn reveal_path(_path: &Path) -> Result<(), String> {
    Err(HINT.to_string())
}

pub(super) fn copy_text(_text: &str) -> Result<(), String> {
    Err(HINT.to_string())
}

pub(super) fn trash_path(_path: &Path) -> Result<(), String> {
    Err(HINT.to_string())
}

pub(super) fn trash_dir() -> Option<PathBuf> {
    None
}

pub(super) fn window_top_left(x: f64, y: f64) -> (f64, f64) {
    (x, y)
}

pub(super) fn app_roots() -> Vec<(PathBuf, &'static str)> {
    Vec::new()
}

pub(super) fn shell_command(cmd: &str) -> (String, Vec<String>) {
    ("sh".to_string(), vec!["-c".to_string(), cmd.to_string()])
}
