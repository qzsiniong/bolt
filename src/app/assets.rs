//! 复合资源源：gpui-kit 内置图标 + 已打开的 UI 扩展目录。
//!
//! gpui-shell 的 `svg(path)` 相对**应用根**解析（不是相对调用它的模块），而一个
//! 进程只有一个全局 `AssetSource`。UI 扩展各有自己的目录，所以这里把所有已打
//! 开的扩展根登记下来，按「最近打开优先」逐个尝试，最后回退到 gpui-kit 的内置
//! 图标全集（编辑器、launcher 用的 Lucide 图标）。
//!
//! 同名资源（例如 `icons/trash.svg`）由**最近打开的扩展**遮蔽内置图标，这一点
//! 写进了 README。

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use gpui_kit::assets::AllAssets;
// 显式导入：`gpui_kit::*` 会把 gpui 的 `#[gpui::test]` 宏带进作用域，遮蔽
// 内置的 `#[test]` 并导致宏展开递归。
use gpui_kit::{AssetSource, SharedString};

/// 应用启动时用 `.with_assets(..)` 安装；clone 一份存进 `AppShell`，开窗时登记根目录。
#[derive(Clone, Default)]
pub(crate) struct ExtensionAssets {
    roots: Arc<parking_lot::Mutex<Vec<PathBuf>>>,
}

impl ExtensionAssets {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 登记一个 UI 扩展根。重复登记只保留一次，并把该根排到最前。
    pub(crate) fn register_root(&self, root: PathBuf) {
        let mut roots = self.roots.lock();
        roots.retain(|known| known != &root);
        roots.push(root);
    }

    /// gpui-kit 内置图标全集（unit struct，按需构造即可）。
    fn kit() -> AllAssets {
        AllAssets::new("")
    }
}

impl AssetSource for ExtensionAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        // 越界或读失败都不算命中：交给下一个候选，最后回退到内置图标。
        for root in self.roots.lock().iter().rev() {
            if let Ok(Some(bytes)) = gpui_shell::AppAssets::new(root.clone()).load(path) {
                return Ok(Some(bytes));
            }
        }
        Self::kit().load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Self::kit().list(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn extension_root_wins_over_builtin_icon() {
        let tmp = TempDir::new("assets-priority");
        // 与内置 Lucide 图标同路径，内容不同，用来断言优先级。
        tmp.write("icons/a-arrow-down.svg", "<svg>extension</svg>");

        let assets = ExtensionAssets::new();
        assets.register_root(tmp.0.clone());

        let bytes = assets
            .load("icons/a-arrow-down.svg")
            .expect("load should not fail")
            .expect("extension asset should be found");
        assert_eq!(&bytes[..], b"<svg>extension</svg>");
    }

    #[test]
    fn builtin_icons_still_resolve() {
        let assets = ExtensionAssets::new();
        let bytes = assets
            .load("icons/trash.svg")
            .expect("builtin icon should load")
            .expect("builtin icon should exist");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn missing_asset_is_an_error_not_a_panic() {
        let assets = ExtensionAssets::new();
        assert!(assets.load("icons/definitely-not-here.svg").is_err());
        // 越界路径既不能读到扩展目录外，也不能 panic。
        let tmp = TempDir::new("assets-traversal");
        tmp.write("secret.svg", "<svg>secret</svg>");
        let assets = ExtensionAssets::new();
        assets.register_root(tmp.0.clone());
        assert!(assets.load("../secret.svg").is_err());
    }
}
