//! [`JsManager`] —— plux `Manager` 的 QuickJS 实现。
//!
//! 生命周期映射：
//!
//! | plux 回调 | 本 crate 的动作 |
//! |---|---|
//! | `register_plugin` | 读 `extension.json`、校验目录名/清单/入口，返回 [`ExtensionMeta`] |
//! | `load_plugin` | 起工人线程、建常驻运行时、求值入口模块 |
//! | `unload_plugin` | 停线程、销毁运行时 |
//!
//! 命令的**执行**不走 plux 的 request 机制：plux 的 request 是"所有扩展必须实现
//! 同一份接口"，而扩展的命令集合各不相同。宿主通过 [`ExtensionStore::invoke`]
//! 直接点命令，`Loader` 只负责生命周期与元数据。

use std::sync::Arc;

use plux_rs::function::FunctionOutput;
use plux_rs::utils::ManagerResult;
use plux_rs::{Api, LoadPluginContext, Manager, Plugin, RegisterPluginContext};

use crate::env::JsEnv;
use crate::manifest;
use crate::meta::ExtensionMeta;
use crate::store::ExtensionStore;

pub struct JsManager {
    store: Arc<ExtensionStore>,
}

impl JsManager {
    pub fn new(env: Arc<dyn JsEnv>) -> Self {
        Self {
            store: ExtensionStore::new(env),
        }
    }

    /// 执行入口。宿主留一份就能在工作线程里调命令。
    pub fn store(&self) -> &Arc<ExtensionStore> {
        &self.store
    }
}

fn boxed(error: impl std::fmt::Display) -> Box<dyn std::error::Error + Send + Sync> {
    Box::<dyn std::error::Error + Send + Sync>::from(error.to_string())
}

impl<'a> Manager<'a, FunctionOutput, ExtensionMeta> for JsManager {
    fn format(&self) -> &'static str {
        manifest::EXTENSION_FORMAT
    }

    fn register_plugin(&mut self, context: RegisterPluginContext) -> ManagerResult<ExtensionMeta> {
        let package = manifest::read(context.path).map_err(boxed)?;
        if package.bundle != *context.bundle {
            return Err(boxed(format!(
                "plux 从目录名解析出 {}，与清单一侧的 {} 不一致",
                context.bundle, package.bundle
            )));
        }
        Ok(ExtensionMeta::new(package))
    }

    fn load_plugin(
        &mut self,
        context: LoadPluginContext<'a, '_, FunctionOutput, ExtensionMeta>,
        _api: Api<FunctionOutput, ExtensionMeta>,
    ) -> ManagerResult<()> {
        let info = context.plugin().info();
        let meta = &info.info;
        // 纯 UI 扩展没有脚本入口，也就没有任何命令需要运行时：登记即完成。
        let Some(entry) = &meta.entry else {
            return Ok(());
        };
        self.store
            .ensure_loaded(&info.bundle, &meta.root, entry, &meta.manifest.permissions)
            .map_err(boxed)
    }

    fn unload_plugin(
        &mut self,
        extension: &Plugin<'a, FunctionOutput, ExtensionMeta>,
    ) -> ManagerResult<()> {
        self.store.unload(&extension.info().bundle);
        Ok(())
    }

    /// plux 注销扩展时会先 `unload_plugin` 再调用这里；`ExtensionStore::unload` 是幂等
    /// 的，兜一次底保证工人线程不会因为某条路径漏了 unload 而残留。
    fn unregister_plugin(
        &mut self,
        extension: &Plugin<'a, FunctionOutput, ExtensionMeta>,
    ) -> ManagerResult<()> {
        self.store.unload(&extension.info().bundle);
        Ok(())
    }
}
