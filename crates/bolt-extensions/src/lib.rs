//! 可复用的 JS（QuickJS）扩展运行时：注册表 + 清单解析 + 常驻引擎。
//!
//! 一个扩展就是一个目录：`extension.json` 声明元数据（id 必填，version 缺省
//! `1.0.0`），目录名只是文件夹名，不参与身份。身份 = 清单里的 `id`，同 id
//! 同一时刻只注册一份。
//!
//! 本 crate 不依赖宿主、不含业务逻辑：console、宿主模块（`import "bolt"` 这类）
//! 与交互 API 全部通过 [`JsEnv`] 注入；执行入口是 [`ExtensionStore`]（`Arc`
//! 可跨线程），生命周期由 [`ExtensionRegistry`] 编排。

pub mod cancel;
pub mod env;
pub mod error;
pub mod fingerprint;
pub mod loader;
pub mod manifest;
pub mod registry;
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
pub use manifest::{
    EXTENSION_SCHEMA, ExtensionCommand, ExtensionManifest, ExtensionPackage, MANIFEST_FILE,
    PERMISSIONS, SCHEMA_FILE, UI_ENTRY, UiAction, UiActionKind, UiBuiltin, UiCapabilities, UiSpec,
    valid_id,
};
pub use registry::{ExtensionRegistry, LoadError, RegisterError, RegisterOutcome, Registration};
pub use store::{DEFAULT_TIMEOUT, ExtensionStore, InvokeRequest, RuntimeState};
