//! 模块解析：插件目录里的文件 + 宿主模块的转发模块。
//!
//! 关键在 [`PluginResolver::resolve`] 把相对路径解析成**绝对路径作为模块名**：
//! rquickjs 只把 `resolve` 的结果当模块名传给 `load`，如果原样透传
//! `"./parsers.js"`，`load` 就失去了"是谁 import 的"这一信息，插件里
//! `lib/a.js` 再 `import "./b.js"` 会解析到插件根而不是 `lib/`。把绝对路径当
//! 模块名既解决了这个问题，也让模块缓存天然正确。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{Ctx, Error, Module, Result};

use crate::env::{HostModuleSpec, JsEnv};

/// 恒等 resolver 的替代品：见模块头部说明，它会把相对说明符变成绝对路径。
pub struct PluginResolver {
    root: PathBuf,
    host_modules: Vec<String>,
}

impl PluginResolver {
    pub fn new(root: PathBuf, host_modules: Vec<String>) -> Self {
        Self { root, host_modules }
    }
}

impl Resolver for PluginResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        // 宿主模块（`import { env } from "bolt"`）原样交给 loader。
        if self.host_modules.iter().any(|module| module == name) {
            return Ok(name.to_string());
        }
        match resolve_file(&self.root, base, name) {
            Some(path) => Ok(path.to_string_lossy().into_owned()),
            // 交给 loader 报"module not found"，那里能给出原始说明符。
            None => Ok(name.to_string()),
        }
    }
}

/// 按模块名加载源码；名字是绝对路径（resolver 的产物）或宿主模块名。
pub struct PluginLoader {
    root: PathBuf,
    env: Arc<dyn JsEnv>,
    /// 宿主模块名 → 按本插件权限过滤后的导出（exports + denied 两半）。
    host_modules: HashMap<String, HostModuleSpec>,
}

impl PluginLoader {
    /// `permissions` 是插件清单里声明的权限（缺省为空 = 全关）。
    pub fn new(root: PathBuf, env: Arc<dyn JsEnv>, permissions: &[String]) -> Self {
        let host_modules = env
            .host_modules(permissions)
            .into_iter()
            .map(|module| (module.name.clone(), module))
            .collect();
        Self {
            root,
            env,
            host_modules,
        }
    }
}

impl Loader for PluginLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attrs: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js>> {
        // 1. 宿主模块 → 转发函数，每次调用都从 globalThis.__<name> 上现取。
        //    不能用 `const { a, b } = globalThis.__x` 解构：宿主模块的导出分两批
        //    安装（install_base 装基础函数，install_scope 在每轮命令前装交互
        //    API），而入口模块在 install_base 之后、第一次 install_scope 之前就
        //    求值了——一次性解构会把当时的 undefined 固化进 const 绑定，插件里
        //    `import { confirm } from 'bolt'` 永远拿到 undefined。
        if let Some(spec) = self.host_modules.get(name) {
            let source = forwarding_module_source(name, &spec.exports, &spec.denied);
            return Module::declare(ctx.clone(), name, source);
        }

        // 2. 插件目录里的文件。resolver 已经给了绝对路径；给出裸名时按根解析。
        let path = Path::new(name);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            resolve_file(&self.root, "", name)
                .ok_or_else(|| Error::new_loading_message(name, "module not found"))?
        };
        let source = std::fs::read_to_string(&path)
            .map_err(|e| Error::new_loading_message(name, e.to_string()))?;
        let code = self
            .env
            .prepare_source(&path, &source)
            .map_err(|e| Error::new_loading_message(name, e))?;
        Module::declare(ctx.clone(), name, code)
    }
}

/// 生成宿主模块的转发模块源码：每个导出名是一个**转发函数**，每次调用都
/// 从 `globalThis.__<name>` 上现取。
///
/// 不能用 `const { a, b } = globalThis.__x` 一次性解构：宿主的导出可能分批
/// 安装（bolt 的基础函数在 `install_base`、交互 API 在每轮命令前的
/// `install_scope`），解构会把求值时刻的 `undefined` 固化进 const 绑定，
/// 后装的函数在 `import` 命名空间里永远是 undefined。
///
/// `denied` 是 `(导出名, 权限名)`：清单没声明对应权限的导出也照常出现在
/// 模块里，但函数体是抛错桩——`import` 不炸，一调用就能看到"清单缺什么
/// 权限"的明确报错，而不是模糊的 `undefined is not a function`。
///
/// 宿主（bolt）的普通脚本 loader 复用同一份生成逻辑（`denied` 传空），
/// 保证两条链路行为一致。
pub fn forwarding_module_source(name: &str, exports: &[String], denied: &[(String, String)]) -> String {
    if exports.is_empty() && denied.is_empty() {
        return "// empty host module".to_string();
    }
    let forwards = exports
        .iter()
        .map(|export| {
            format!("export function {export}(...args) {{ return globalThis.__{name}.{export}(...args); }}")
        })
        .chain(denied.iter().map(|(export, permission)| {
            format!(
                "export function {export}(...args) {{ throw new Error('插件未声明权限 \"{permission}\"：请在 extension.json 的 permissions 中加入 \"{permission}\"'); }}"
            )
        }))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{forwards}\n")
}

/// 把一个说明符解析到磁盘文件；相对说明符以 `base` 所在目录为基准。
///
/// 顺序尝试 `<path>` / `<path>.js` / `<path>.ts` / `<path>/index.js` /
/// `<path>/index.ts`。宿主的单文件脚本 loader 复用这一份（`base` 传空串，
/// 相对说明符即以 `root` 为基准）。
pub fn resolve_file(root: &Path, base: &str, name: &str) -> Option<PathBuf> {
    let joined = if name.starts_with('.') {
        let base_dir = Path::new(base)
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());
        base_dir.join(name.trim_start_matches("./"))
    } else if Path::new(name).is_absolute() {
        PathBuf::from(name)
    } else {
        root.join(name)
    };

    let candidates = [
        joined.clone(),
        joined.with_extension("js"),
        joined.with_extension("ts"),
        joined.join("index.js"),
        joined.join("index.ts"),
    ];
    candidates.into_iter().find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn resolves_relative_specifiers_against_the_importing_file() {
        let tmp = TempDir::new("loader-relative");
        tmp.write("lib/a.js", "");
        tmp.write("lib/b.js", "");
        tmp.write("index.js", "");

        let base = tmp.0.join("lib/a.js").to_string_lossy().into_owned();
        let resolved = resolve_file(&tmp.0, &base, "./b.js").expect("sibling");
        assert_eq!(resolved, tmp.0.join("lib/b.js"));

        let resolved = resolve_file(&tmp.0, "index.js", "./index.js").expect("root file");
        assert_eq!(resolved, tmp.0.join("index.js"));
    }

    #[test]
    fn bare_specifiers_resolve_under_the_plugin_root() {
        let tmp = TempDir::new("loader-bare");
        tmp.write("index.js", "");
        tmp.write("pkg/index.js", "");

        assert_eq!(
            resolve_file(&tmp.0, "", "index.js"),
            Some(tmp.0.join("index.js"))
        );
        assert_eq!(
            resolve_file(&tmp.0, "", "pkg"),
            Some(tmp.0.join("pkg/index.js"))
        );
        assert_eq!(resolve_file(&tmp.0, "", "missing.js"), None);
    }

    #[test]
    fn denied_exports_become_throwing_stubs() {
        let source = forwarding_module_source(
            "bolt",
            &["confirm".to_string()],
            &[("readFile".to_string(), "fs".to_string())],
        );
        assert!(
            source.contains("return globalThis.__bolt.confirm(...args);"),
            "allowed export must forward: {source}"
        );
        assert!(
            source.contains("throw new Error('插件未声明权限 \"fs\""),
            "denied export must throw with the permission name: {source}"
        );
    }

    #[test]
    fn all_denied_still_emits_a_module() {
        let source = forwarding_module_source(
            "bolt",
            &[],
            &[("fetch".to_string(), "net".to_string())],
        );
        assert!(
            source.contains("export function fetch"),
            "denied-only module must still export the name: {source}"
        );
    }
}
