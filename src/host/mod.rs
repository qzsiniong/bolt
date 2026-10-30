//! HostModule framework — register Rust modules that scripts can `import`.
//!
//! Each `HostModule` has a name, a TypeScript declaration (written to
//! `scripts/types/<name>.d.ts` for IDE/LSP), a list of export names, and an
//! installer closure that creates native functions on a global object.
//!
//! At runtime the [`HostLoader`] (impl `rquickjs::Loader`) intercepts
//! `import "name"`: for a registered host module it declares a tiny re-export
//! module that pulls functions off a global namespace; for file specifiers it
//! reads + (for `.ts`) type-strips the source.

pub(crate) mod modules;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{Ctx, Error, Module, Result};

use crate::runtime::ts_transform::strip_typescript;

/// Installer closure: registers a module's native functions on its global
/// namespace object inside the QuickJS context.
type Installer = Box<dyn for<'js> Fn(&Ctx<'js>) -> Result<()> + Send + Sync>;

/// A Rust module exposed to scripts via `import { ... } from "name"`.
pub struct HostModule {
    pub name: String,
    declaration: String,
    exports: Vec<String>,
    /// 导出名 → 所需权限（见 `plux_js_manager::PERMISSIONS`）。没登记的导出
    /// 不需要声明。插件运行时用它把导出拆成"允许 / 拒绝"两半；普通脚本链路
    /// 不看它（脚本 = 用户自己写的代码，全权）。
    required: HashMap<String, String>,
    installer: Installer,
}

impl HostModule {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            declaration: String::new(),
            exports: Vec::new(),
            required: HashMap::new(),
            installer: Box::new(|_| Ok(())),
        }
    }

    /// TypeScript declaration body (the contents inside `declare module "name" { … }`).
    pub fn declaration(mut self, decl: impl Into<String>) -> Self {
        self.declaration = decl.into();
        self
    }

    /// Add an exported name. Call once per export.
    pub fn export(mut self, name: impl Into<String>) -> Self {
        self.exports.push(name.into());
        self
    }

    /// Add an exported name guarded by a permission: 插件清单不声明该权限时，
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

    /// Installer called inside the QuickJS context to register native
    /// functions on a global object named `__<module_name>`.
    pub fn installer<F>(mut self, f: F) -> Self
    where
        F: for<'js> Fn(&Ctx<'js>) -> Result<()> + Send + Sync + 'static,
    {
        self.installer = Box::new(f);
        self
    }
}

/// Registry of all host modules. Cheaply cloneable behind `Arc`.
pub struct HostRegistry {
    pub modules: HashMap<String, HostModule>,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    pub fn register(&mut self, module: HostModule) {
        self.modules.insert(module.name.clone(), module);
    }

    /// Write `declare module "name" { … }` for every module to `types_dir`.
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
            let _ = std::fs::write(path, content); // best-effort
        }
        Ok(())
    }

    /// Install all module globals on the given context. Called by the runner
    /// before evaluating the user script.
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

// ─── Loader + Resolver ─────────────────────────────────────────────────────

/// rquickjs `Loader`: host modules first, then file-based modules.
pub struct HostLoader {
    pub registry: Arc<HostRegistry>,
    pub scripts_dir: PathBuf,
}

impl HostLoader {
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
        // 1. Registered host module → forward to the global namespace.
        //    转发源码由 plux-js-manager 统一生成（转发函数每次调用都现取
        //    `globalThis.__<name>`，插件与脚本两条链路行为一致）。脚本链路
        //    是用户自己写的代码，全权：denied 恒为空。
        if let Some(module) = self.registry.modules.get(name) {
            let source =
                plux_js_manager::forwarding_module_source(&module.name, &module.exports, &[]);
            return Module::declare(ctx.clone(), name, source);
        }

        // 2. File-based module (relative path or bare path).
        //    单文件脚本没有"导入方"概念，`base` 传空串：相对说明符以脚本
        //    目录为基准。解析逻辑与插件运行时共用同一份。
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

/// Identity resolver — the loader handles both host names and paths.
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
