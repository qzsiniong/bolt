//! plux-rs 的 JavaScript（QuickJS）扩展管理器。
//!
//! 与官方 `plux-lua-manager` 对应：实现 [`plux_rs::Manager`]，把扩展目录里的
//! `extension.json` 解析成 [`ExtensionMeta`]，并为每个扩展维护一个**常驻**的 QuickJS
//! 运行时（专用线程 + 消息驱动），命令之间保留模块状态。
//!
//! 本 crate 不依赖宿主：console、宿主模块（`import "bolt"`）与交互 API 全部通过
//! [`JsEnv`] 注入，执行入口是 [`ExtensionStore`]（`Arc` 可跨线程）。

pub mod cancel;
pub mod env;
pub mod error;
pub mod fingerprint;
pub mod loader;
pub mod manager;
pub mod manifest;
pub mod meta;
pub mod schema;
pub mod store;
pub mod value;

mod worker;

#[cfg(test)]
mod test_support;

pub use cancel::CancelToken;
pub use env::{EmptyEnv, HostModuleSpec, JsEnv, LogLevel, Scope};
pub use error::JsError;
pub use fingerprint::dir_fingerprint;
pub use loader::{forwarding_module_source, resolve_file};
pub use manager::JsManager;
pub use manifest::{
    EXTENSION_FORMAT, EXTENSION_SCHEMA, ExtensionCommand, ExtensionManifest, ExtensionPackage,
    MANIFEST_FILE, PERMISSIONS, SCHEMA_FILE, UI_ENTRY, UiAction, UiActionKind, UiBuiltin,
    UiCapabilities, UiSpec,
};
pub use meta::ExtensionMeta;
pub use store::{DEFAULT_TIMEOUT, ExtensionStore, InvokeRequest, RuntimeState, bundle_key};
