//! 宿主注入钩子。
//!
//! 本 crate 不认识 bolt：console、宿主模块（`import "bolt"`）、交互 API 与
//! TypeScript 预处理全部由宿主通过 [`JsEnv`] 提供。这样 manager 既能在应用里
//! 用，也能在测试里用一个最小实现跑起来。

use std::any::Any;
use std::path::Path;

use rquickjs::Ctx;

/// 宿主模块（脚本里 `import { env } from "bolt"` 的那个 `bolt`）。
///
/// 模块本体由宿主在 `install_base` 里挂到 `globalThis.__<name>` 上；loader 只负责
/// 生成一个转发模块，把 `exports` 里的名字重新导出。
#[derive(Clone, Debug)]
pub struct HostModuleSpec {
    pub name: String,
    pub exports: Vec<String>,
}

/// 日志级别：宿主要把它映射成 Console 面板的样式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// 每次命令调用携带的宿主侧上下文（交互 bridge、取消令牌等）。
///
/// crate 只把它当不透明负载透传：`install_scope` 收到后由宿主自己 downcast 成
/// 具体类型（`bolt` 侧是 `RunScope`），这样 crate 不需要知道交互 API 的形状。
pub trait Scope: Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
}

/// 宿主环境。
pub trait JsEnv: Send + Sync + 'static {
    /// 插件加载时调用一次：装 console、定时器、宿主模块的全局对象。
    fn install_base<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<()>;

    /// 每条命令执行前调用：把本轮的交互 API（confirm / pickPath / progress /
    /// typeStr）挂到扩展上下文上。实现应当**覆盖**上一次安装的函数——插件运行时
    /// 是常驻的，而交互 bridge 是每轮新建的。
    fn install_scope<'js>(&self, ctx: &Ctx<'js>, scope: &dyn Scope) -> rquickjs::Result<()>;

    /// 脚本可以 `import` 的宿主模块清单。
    fn host_modules(&self) -> Vec<HostModuleSpec>;

    /// 源码预处理：`.ts` 去类型。`.js` 原样返回。
    fn prepare_source(&self, path: &Path, code: &str) -> Result<String, String>;

    /// 插件自己 `console.log` 的输出。
    fn log(&self, level: LogLevel, message: &str);
}

/// 什么也不做的环境，供只关心清单/生命周期的测试使用。
#[derive(Default, Debug)]
pub struct EmptyEnv;

impl JsEnv for EmptyEnv {
    fn install_base<'js>(&self, _ctx: &Ctx<'js>) -> rquickjs::Result<()> {
        Ok(())
    }

    fn install_scope<'js>(&self, _ctx: &Ctx<'js>, _scope: &dyn Scope) -> rquickjs::Result<()> {
        Ok(())
    }

    fn host_modules(&self) -> Vec<HostModuleSpec> {
        Vec::new()
    }

    fn prepare_source(&self, _path: &Path, code: &str) -> Result<String, String> {
        Ok(code.to_string())
    }

    fn log(&self, _level: LogLevel, _message: &str) {}
}
