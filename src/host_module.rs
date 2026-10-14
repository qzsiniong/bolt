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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::{Ctx, Error, Module, Result};

use crate::ts_transform::strip_typescript;

/// A Rust module exposed to scripts via `import { ... } from "name"`.
pub struct HostModule {
    pub name: String,
    declaration: String,
    exports: Vec<String>,
    installer: Box<dyn for<'js> Fn(&Ctx<'js>) -> Result<()> + Send + Sync>,
}

impl HostModule {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            declaration: String::new(),
            exports: Vec::new(),
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
        // 1. Registered host module → re-export from the global namespace.
        if let Some(module) = self.registry.modules.get(name) {
            let exports = module.exports.join(", ");
            let global = format!("__{}", module.name);
            let source = if exports.is_empty() {
                "// empty host module".to_string()
            } else {
                format!("const {{ {exports} }} = globalThis.{global};\nexport {{ {exports} }};\n")
            };
            return Module::declare(ctx.clone(), name, source);
        }

        // 2. File-based module (relative path or bare path).
        let path = resolve_file(&self.scripts_dir, name)
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

/// Try to resolve a module specifier to a file on disk.
fn resolve_file(scripts_dir: &Path, name: &str) -> Option<PathBuf> {
    // Absolute or rooted relative path.
    let p = Path::new(name);
    let candidates: Vec<PathBuf> = if p.is_absolute() || name.starts_with('.') {
        let base = if name.starts_with('.') {
            scripts_dir.to_path_buf()
        } else {
            PathBuf::new()
        };
        let joined = base.join(name.trim_start_matches("./"));
        vec![
            joined.clone(),
            joined.with_extension("js"),
            joined.with_extension("ts"),
            joined.join("index.js"),
            joined.join("index.ts"),
        ]
    } else {
        // Bare path relative to scripts dir.
        let joined = scripts_dir.join(name);
        vec![
            joined.clone(),
            joined.with_extension("js"),
            joined.with_extension("ts"),
        ]
    };
    candidates.into_iter().find(|c| c.is_file())
}
