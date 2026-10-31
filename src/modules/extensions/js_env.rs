//! 宿主注入：把 bolt 的 console、宿主模块与交互 bridge 接进 plux 的 JS 运行时。
//!
//! `plux-js-manager` 不认识 bolt，它只要求实现 [`JsEnv`]：
//!
//! - `install_base` 在扩展加载时调用一次（console / 定时器 / `import "bolt"`）；
//! - `install_scope` 在**每条命令前**调用：扩展运行时是常驻的，而交互 bridge 每轮
//!   新建，所以这里要覆盖上一轮装进去的 `__bolt.confirm` 等函数。

use std::any::Any;
use std::path::Path;
use std::sync::Arc;

use plux_js_manager::{HostModuleSpec, JsEnv, JsError, LogLevel, Scope};
use rquickjs::Ctx;

use crate::host::HostRegistry;
use crate::host::modules::bolt;
use crate::runtime::interaction::InteractionBridge;
use bolt_console::ConsoleChannel;
use bolt_model::script::Language;
use bolt_ts::strip_typescript;

/// bolt 侧的运行时环境：一套宿主模块 + 一个 Console 通道。
pub(crate) struct BoltJsEnv {
    pub(crate) registry: Arc<HostRegistry>,
    pub(crate) console: ConsoleChannel,
}

impl JsEnv for BoltJsEnv {
    fn install_base<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<()> {
        // 顺序与普通脚本一致：console 先装（后面的安装失败也能报出来），
        // 再装定时器与宿主模块的全局对象。
        bolt::install_console(ctx, &self.console)?;
        bolt::install_timers(ctx, &self.console)?;
        self.registry.install_globals(ctx)?;
        Ok(())
    }

    fn install_scope<'js>(&self, ctx: &Ctx<'js>, scope: &dyn Scope) -> rquickjs::Result<()> {
        let Some(run) = scope.as_any().downcast_ref::<ExtensionRunScope>() else {
            return Ok(());
        };
        // Interaction APIs（typeStr / requestFocus / confirm / pickPath / scanQr）
        // 扩展的是 install_base 里建好的 `__bolt` 对象。
        bolt::install_interaction(ctx, &run.bridge)
    }

    fn host_modules(&self, permissions: &[String]) -> Vec<HostModuleSpec> {
        self.registry
            .modules
            .values()
            .map(|module| {
                let (exports, denied) = module.exports_for(permissions);
                HostModuleSpec {
                    name: module.name.clone(),
                    exports,
                    denied,
                }
            })
            .collect()
    }

    fn prepare_source(&self, path: &Path, code: &str) -> Result<String, String> {
        match Language::from_path(path) {
            Some(Language::TypeScript) => strip_typescript(code),
            _ => Ok(code.to_string()),
        }
    }

    fn log(&self, level: LogLevel, message: &str) {
        match level {
            LogLevel::Info => self.console.info(message),
            LogLevel::Warn => self.console.warn(message),
            LogLevel::Error => self.console.error(message),
        }
    }

    /// 扩展命令里的 `exit(code)` 与脚本路径同义：抛哨兵正常结束，不是失败。
    /// 哨兵格式由 bolt 自己定义，解析也只在这一处。
    fn classify_error(&self, message: &str) -> Option<JsError> {
        bolt::exit_code(message).map(JsError::Exit)
    }
}

/// 一轮命令调用的交互上下文：RunService 每轮新建的 bridge。
#[derive(Clone)]
pub(crate) struct ExtensionRunScope {
    pub(crate) bridge: InteractionBridge,
}

impl Scope for ExtensionRunScope {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
