//! plux-rs 的 JavaScript（QuickJS）插件管理器。
//!
//! 与官方 `plux-lua-manager` 对应：实现 [`plux_rs::Manager`]，把插件目录里的
//! `extension.json` 解析成 [`PluginMeta`]，并为每个插件维护一个**常驻**的 QuickJS
//! 运行时（专用线程 + 消息驱动），命令之间保留模块状态。
//!
//! 本 crate 不依赖宿主：console、宿主模块（`import "bolt"`）与交互 API 全部通过
//! [`JsEnv`] 注入，执行入口是 [`PluginStore`]（`Arc` 可跨线程）。

pub mod cancel;
pub mod env;
pub mod error;
pub mod loader;
pub mod manager;
pub mod manifest;
pub mod meta;
pub mod store;
pub mod value;

mod worker;

#[cfg(test)]
mod test_support;

pub use cancel::CancelToken;
pub use env::{EmptyEnv, HostModuleSpec, JsEnv, LogLevel, Scope};
pub use error::JsError;
pub use loader::{forwarding_module_source, resolve_file};
pub use manager::JsManager;
pub use manifest::{
    MANIFEST_FILE, PLUGIN_FORMAT, PluginCommand, PluginManifest, PluginPackage, UI_ENTRY,
    UiCapabilities, UiSpec,
};
pub use meta::PluginMeta;
pub use store::{DEFAULT_TIMEOUT, InvokeRequest, PluginStore, bundle_key};
