//! [`ExtensionMeta`] —— 挂在 plux `ExtensionInfo.info` 上的自定义扩展信息。
//!
//! plux 的 `Info` trait 只要求依赖列表，宿主需要的 `commands` / `name` /
//! 目录这些字段一并挂在这里：`Loader::get_plugins()` 拿到 `ExtensionInfo`，再读
//! `info.info.manifest` 就能生成 Launcher item，不必另存一份注册表。

use std::path::PathBuf;

use plux_rs::{Depend, Info};

use crate::manifest::{ExtensionCommand, ExtensionManifest, ExtensionPackage};

#[derive(Clone, Debug)]
pub struct ExtensionMeta {
    pub manifest: ExtensionManifest,
    /// 扩展目录（绝对路径）：UI 命令用它开 gpui-shell 窗口。
    pub root: PathBuf,
    /// 脚本入口（绝对路径）；纯 UI 扩展为 `None`。
    pub entry: Option<PathBuf>,
    /// UI 入口；没有 `ui` 命令时为 `None`。
    pub main: Option<PathBuf>,
    depends: Vec<Depend>,
    optional_depends: Vec<Depend>,
}

impl ExtensionMeta {
    pub fn new(package: ExtensionPackage) -> Self {
        Self {
            manifest: package.manifest,
            root: package.root,
            entry: package.entry,
            main: package.main,
            depends: package.depends,
            optional_depends: package.optional_depends,
        }
    }

    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn display_name(&self) -> &str {
        self.manifest.display_name()
    }

    pub fn commands(&self) -> &[ExtensionCommand] {
        &self.manifest.commands
    }
}

impl Info for ExtensionMeta {
    fn depends(&self) -> &Vec<Depend> {
        &self.depends
    }

    fn optional_depends(&self) -> &Vec<Depend> {
        &self.optional_depends
    }
}
