//! 应用数据目录的唯一事实来源。
//!
//! 全部磁盘路径都从 [`data_root`] 派生；任何地方都不允许再各自 hardcode
//! 一层 `"bolt"`，否则改应用名或数据目录时容易漏改。

use std::path::PathBuf;

/// 应用数据根目录：`~/Library/Application Support/bolt/`
/// （Windows 上是 `%APPDATA%\bolt\`，Linux 上是 `~/.local/share/bolt/`）。
///
/// 取不到系统数据目录时退回当前目录 `.`，保证进程还能启动（各写入方再
/// 各自兜底报错）。
pub fn data_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bolt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_root_ends_with_bolt() {
        let root = data_root();
        assert_eq!(root.file_name().and_then(|n| n.to_str()), Some("bolt"));
    }
}
