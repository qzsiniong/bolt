//! HostModule 框架——把 Rust 模块注册成脚本可 `import` 的宿主模块。
//!
//! 每个 `HostModule` 包含：名字、TypeScript 声明（写到
//! `scripts/types/<name>.d.ts` 供 IDE/LSP 使用）、导出名列表，以及一个安装闭包
//! （在全局对象上创建原生函数）。
//!
//! 运行时由 [`HostLoader`]（实现 `rquickjs::Loader`）拦截 `import "name"`：
//! 命中已注册的宿主模块时，声明一个极小的再导出模块，从全局命名空间取函数；
//! 命中文件说明符时读取源文件，`.ts` 还会先剥掉类型。

pub(crate) mod modules;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{Ctx, Error, Module, Result};

use crate::runtime::ts_transform::strip_typescript;

/// 安装闭包：在 QuickJS 上下文里，把模块的原生函数挂到它的全局命名空间对象上。
type Installer = Box<dyn for<'js> Fn(&Ctx<'js>) -> Result<()> + Send + Sync>;

/// 通过 `import { ... } from "name"` 暴露给脚本的 Rust 模块。
pub struct HostModule {
    pub name: String,
    declaration: String,
    exports: Vec<String>,
    /// 导出名 → 所需权限（见 `plux_js_manager::PERMISSIONS`）。没登记的导出
    /// 不需要声明。扩展运行时用它把导出拆成"允许 / 拒绝"两半；普通脚本链路
    /// 不看它（脚本 = 用户自己写的代码，全权）。
    required: HashMap<String, String>,
    installer: Installer,
}

impl HostModule {
    /// 新建一个空模块：无声明、无导出、安装闭包为空操作。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            declaration: String::new(),
            exports: Vec::new(),
            required: HashMap::new(),
            installer: Box::new(|_| Ok(())),
        }
    }

    /// TypeScript 声明正文（即 `declare module "name" { … }` 大括号里的内容）。
    pub fn declaration(mut self, decl: impl Into<String>) -> Self {
        self.declaration = decl.into();
        self
    }

    /// 添加一个导出名，每个导出调用一次。
    pub fn export(mut self, name: impl Into<String>) -> Self {
        self.exports.push(name.into());
        self
    }

    /// 添加一个受权限保护的导出名：扩展清单不声明该权限时，
    /// 运行时里这个导出是调用即抛错的占位函数。
    pub fn export_guarded(mut self, name: impl Into<String>, permission: &str) -> Self {
        let name = name.into();
        self.required.insert(name.clone(), permission.to_string());
        self.exports.push(name);
        self
    }

    /// 按权限拆导出：返回 `(允许, 拒绝)`。拒绝项带所需权限名，供转发模块
    /// 生成抛错桩。未登记权限的导出一律放行。
    pub fn exports_for(&self, permissions: &[String]) -> (Vec<String>, Vec<(String, String)>) {
        let mut allowed = Vec::new();
        let mut denied = Vec::new();
        for export in &self.exports {
            match self.required.get(export) {
                Some(permission) if !permissions.contains(permission) => {
                    denied.push((export.clone(), permission.clone()));
                }
                _ => allowed.push(export.clone()),
            }
        }
        (allowed, denied)
    }

    /// 在 QuickJS 上下文内调用的安装器：把原生函数挂到名为 `__<模块名>` 的
    /// 全局对象上。
    pub fn installer<F>(mut self, f: F) -> Self
    where
        F: for<'js> Fn(&Ctx<'js>) -> Result<()> + Send + Sync + 'static,
    {
        self.installer = Box::new(f);
        self
    }
}

/// 全部宿主模块的注册表。外面套 `Arc` 后可廉价克隆。
pub struct HostRegistry {
    pub modules: HashMap<String, HostModule>,
}

impl HostRegistry {
    /// 新建空注册表。
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    /// 注册（或按名覆盖）一个模块。
    pub fn register(&mut self, module: HostModule) {
        self.modules.insert(module.name.clone(), module);
    }

    /// 为每个模块把 `declare module "name" { … }` 写到 `types_dir`。
    pub fn write_type_declarations(&self, types_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(types_dir)?;
        for module in self.modules.values() {
            let body = if module.declaration.is_empty() {
                module
                    .exports
                    .iter()
                    .map(|n| format!("export const {n}: any;"))
                    .collect::<Vec<_>>()
                    .join("\n  ")
            } else {
                module.declaration.trim().to_string()
            };
            let content = format!("declare module \"{}\" {{\n  {}\n}}\n", module.name, body);
            let path = types_dir.join(format!("{}.d.ts", module.name));
            let _ = std::fs::write(path, content); // 尽力而为：写失败只影响 IDE 提示
        }
        Ok(())
    }

    /// 在给定上下文里装上所有模块的全局对象。runner 在求值用户脚本前调用。
    pub fn install_globals<'js>(&self, ctx: &Ctx<'js>) -> Result<()> {
        for module in self.modules.values() {
            (module.installer)(ctx)?;
        }
        Ok(())
    }
}

impl Default for HostRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── 加载器与解析器（Loader / Resolver） ────────────────────────────────────

/// rquickjs 的 `Loader`：先查宿主模块，再落到文件模块。
pub struct HostLoader {
    pub registry: Arc<HostRegistry>,
    pub scripts_dir: PathBuf,
}

impl HostLoader {
    /// 绑定注册表与脚本根目录。
    pub fn new(registry: Arc<HostRegistry>, scripts_dir: PathBuf) -> Self {
        Self {
            registry,
            scripts_dir,
        }
    }
}

impl Loader for HostLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js>> {
        // 1. 已注册的宿主模块 → 转发到全局命名空间。
        //    转发源码由 plux-js-manager 统一生成（转发函数每次调用都现取
        //    `globalThis.__<name>`，扩展与脚本两条链路行为一致）。脚本链路
        //    是用户自己写的代码，全权：denied 恒为空。
        if let Some(module) = self.registry.modules.get(name) {
            let source =
                plux_js_manager::forwarding_module_source(&module.name, &module.exports, &[]);
            return Module::declare(ctx.clone(), name, source);
        }

        // 2. 文件模块（相对路径或裸路径）。
        //    单文件脚本没有"导入方"概念，`base` 传空串：相对说明符以脚本
        //    目录为基准。解析逻辑与扩展运行时共用同一份。
        let path = plux_js_manager::resolve_file(&self.scripts_dir, "", name)
            .ok_or_else(|| Error::new_loading_message(name, "module not found"))?;
        let source = std::fs::read_to_string(&path)
            .map_err(|e| Error::new_loading_message(name, e.to_string()))?;
        let js = if path.extension().is_some_and(|ext| ext == "ts") {
            strip_typescript(&source).map_err(|e| Error::new_loading_message(name, e))?
        } else {
            source
        };
        Module::declare(ctx.clone(), name, js)
    }
}

/// 恒等解析器——宿主名与路径都由加载器自己处理，这里原样返回说明符。
pub struct HostResolver;

impl Resolver for HostResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        _base: &str,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        Ok(name.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDir, run_js, run_with_registry};

    #[test]
    fn type_declarations_cover_every_registered_module() {
        let mut registry = HostRegistry::new();
        modules::register_all(&mut registry);
        let names: Vec<&String> = registry.modules.keys().collect();
        assert!(
            names.iter().any(|n| n.as_str() == "bolt"),
            "built-in bolt module must be registered: {names:?}"
        );

        let tmp = TempDir::new("host-decls");
        registry.write_type_declarations(&tmp.0).unwrap();
        let bolt = std::fs::read_to_string(tmp.0.join("bolt.d.ts")).unwrap();
        assert!(bolt.starts_with("declare module \"bolt\" {"), "{bolt}");
        assert!(bolt.contains("export function readFile"), "{bolt}");
    }

    #[test]
    fn host_module_import_reaches_the_bolt_globals() {
        let tmp = TempDir::new("host-import");
        let lines = run_js(
            "import { listDir } from 'bolt'; console.log(typeof listDir);",
            &tmp.0,
        );
        assert!(
            lines.iter().any(|l| l.contains("function")),
            "host module export missing: {lines:?}"
        );
    }

    #[test]
    fn file_module_import_resolves_relative_paths() {
        let tmp = TempDir::new("host-file-import");
        tmp.write(
            "helper.js",
            "export function hi() { return 'from helper'; }",
        );
        let lines = run_js(
            "import { hi } from './helper.js'; console.log(hi());",
            &tmp.0,
        );
        assert!(
            lines.iter().any(|l| l.contains("from helper")),
            "file module import failed: {lines:?}"
        );
    }

    #[test]
    fn ts_module_import_is_type_stripped() {
        let tmp = TempDir::new("host-ts-import");
        tmp.write(
            "greeter.ts",
            "export const greet = (name: string): string => `hi ${name}`;",
        );
        // 裸说明符（无扩展名）也要能解析到 .ts。
        let lines = run_js(
            "import { greet } from 'greeter'; console.log(greet('ts'));",
            &tmp.0,
        );
        assert!(
            lines.iter().any(|l| l.contains("hi ts")),
            "ts module import failed: {lines:?}"
        );
    }

    #[test]
    fn missing_module_is_reported_not_fatal_to_the_engine() {
        let tmp = TempDir::new("host-missing-import");
        let lines = run_js("import './nope.js';", &tmp.0);
        assert!(
            lines.iter().any(|l| l.contains("module not found")),
            "missing module must surface in console: {lines:?}"
        );
    }

    #[test]
    fn loader_prefers_host_modules_over_files() {
        let tmp = TempDir::new("host-precedence");
        // 磁盘上有一个与宿主模块同名的文件：宿主模块必须赢。
        tmp.write("bolt.js", "export const hijacked = true;");
        let mut registry = HostRegistry::new();
        modules::register_all(&mut registry);
        let lines = run_with_registry(
            "import * as bolt from 'bolt'; console.log(typeof bolt.readFile);",
            &tmp.0,
            std::sync::Arc::new(registry),
        );
        assert!(
            lines.iter().any(|l| l.contains("function")),
            "host module must win over a same-named file: {lines:?}"
        );
    }
}
