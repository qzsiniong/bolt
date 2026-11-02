//! 扩展注册表：登记、注销与懒加载编排。
//!
//! 身份 = 清单里的 `id`（见 [`crate::manifest`]），同 id 同一时刻只注册一份。
//! [`ExtensionRegistry::register`] 是幂等的：同一目录重复注册且内容没变时返回
//! [`RegisterOutcome::Unchanged`]，什么都不动；内容变了（目录指纹变化）则先卸载
//! 旧运行时再登记新清单，返回 [`RegisterOutcome::Reloaded`]——旧实现里
//! 「指纹变化 → 注销重注册」的宿主逻辑已并入这里。
//!
//! 运行时的启停全部委托 [`ExtensionStore`]：注册表只维护「哪些扩展存在、元数据
//! 是什么、内容指纹是多少」。进程退出时 `ExtensionStore::drop` 会收掉全部工人
//! 线程，注册表因此不需要自己的 `Drop` 兜底。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::fingerprint::dir_fingerprint;
use crate::manifest::ExtensionPackage;
use crate::store::ExtensionStore;

/// 一次 [`ExtensionRegistry::register`] 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// 新 id：首次登记。
    Added,
    /// 已有同 id、同目录、内容指纹未变：什么都不动。
    Unchanged,
    /// 已有同 id、同目录，但内容指纹变了：旧运行时已卸载，新清单已登记，
    /// 下次 [`ExtensionRegistry::load`] 会重建运行时。
    Reloaded,
}

/// 注册失败。
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    /// 清单读取 / 解析 / 校验失败，`message` 是中文原因。
    #[error("{message}（{dir}）")]
    Manifest { dir: PathBuf, message: String },
    /// 另一个目录已经登记了同一个 id。同 id 只能有一份，磁盘上两份属作者错误。
    #[error("`{id}` 已注册（{existing_root}）；同 id 只能有一份，请先移除多余目录")]
    AlreadyRegistered { id: String, existing_root: PathBuf },
}

/// 懒加载失败。
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("扩展 `{0}` 未注册")]
    NotRegistered(String),
    /// 运行时线程拉不起来（清单校验在 register 已完成，这里的失败只剩线程错误）。
    #[error("{0}")]
    Runtime(#[from] crate::error::JsError),
}

/// 注册表里的一条登记：校验通过的清单 + 注册时的目录指纹。
#[derive(Clone, Debug)]
pub struct Registration {
    pub package: ExtensionPackage,
    /// 注册时的 [`dir_fingerprint`]；目录不可读时为 `None`（下次 register 视为已变）。
    pub fingerprint: Option<u64>,
}

impl Registration {
    pub fn id(&self) -> &str {
        self.package.id()
    }

    pub fn root(&self) -> &Path {
        &self.package.root
    }
}

/// 扩展注册表。`register`/`unregister` 需要 `&mut`（单线程编排）；执行入口
/// [`ExtensionStore`] 通过 `Arc` 跨线程共享。
pub struct ExtensionRegistry {
    store: Arc<ExtensionStore>,
    /// 按 id 有序：宿主展示顺序稳定，不依赖目录扫描顺序。
    entries: BTreeMap<String, Registration>,
}

impl ExtensionRegistry {
    pub fn new(store: Arc<ExtensionStore>) -> Self {
        Self {
            store,
            entries: BTreeMap::new(),
        }
    }

    /// 登记一个扩展目录（读清单 → 校验 → 查重 → 记指纹）。
    ///
    /// 幂等：同 id 同目录重复注册时，按内容指纹返回 [`RegisterOutcome::Unchanged`]
    /// 或 [`RegisterOutcome::Reloaded`]。指纹变化意味着**任何**文件（清单、入口、
    /// 资产）变了，此时旧运行时先卸载——运行时的重建交给调用方择机 [`Self::load`]。
    pub fn register(&mut self, dir: &Path) -> Result<RegisterOutcome, RegisterError> {
        let package = crate::manifest::read(dir).map_err(|message| RegisterError::Manifest {
            dir: dir.to_path_buf(),
            message,
        })?;
        let id = package.id().to_string();
        let fingerprint = dir_fingerprint(dir);

        let mut replaced = false;
        if let Some(existing) = self.entries.get(&id) {
            if existing.package.root != package.root {
                return Err(RegisterError::AlreadyRegistered {
                    id,
                    existing_root: existing.package.root.clone(),
                });
            }
            if existing.fingerprint.is_some() && existing.fingerprint == fingerprint {
                return Ok(RegisterOutcome::Unchanged);
            }
            // 内容变了：旧运行时必须先收掉（它还挂着旧代码），再登记新清单。
            replaced = true;
            self.store.unload(&id);
        }

        self.entries.insert(
            id,
            Registration {
                package,
                fingerprint,
            },
        );
        Ok(if replaced {
            RegisterOutcome::Reloaded
        } else {
            RegisterOutcome::Added
        })
    }

    /// 注销一个扩展：卸载运行时 + 摘表。未注册时静默返回 `false`（幂等）。
    pub fn unregister(&mut self, id: &str) -> bool {
        if self.entries.remove(id).is_some() {
            self.store.unload(id);
            true
        } else {
            false
        }
    }

    /// 懒加载：确保扩展的运行时已安排启动。**不阻塞**，就绪与否由
    /// [`ExtensionStore::state`] 暴露。纯 UI 扩展（没有脚本入口）登记即完成。
    pub fn load(&self, id: &str) -> Result<(), LoadError> {
        let Some(registration) = self.entries.get(id) else {
            return Err(LoadError::NotRegistered(id.to_string()));
        };
        let Some(entry) = &registration.package.entry else {
            return Ok(());
        };
        self.store
            .ensure_loaded(
                id,
                registration.package.version(),
                &registration.package.root,
                entry,
                &registration.package.manifest.permissions,
            )
            .map_err(LoadError::from)
    }

    /// 卸载运行时（不注销登记）。未注册 / 未加载都是 no-op。
    pub fn unload(&self, id: &str) {
        self.store.unload(id);
    }

    pub fn get(&self, id: &str) -> Option<&Registration> {
        self.entries.get(id)
    }

    /// 按 id 有序遍历全部登记。
    pub fn all(&self) -> impl Iterator<Item = (&str, &Registration)> {
        self.entries.iter().map(|(id, r)| (id.as_str(), r))
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 执行入口（`Arc` 可跨线程共享）。
    pub fn store(&self) -> &Arc<ExtensionStore> {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::EmptyEnv;
    use crate::manifest::MANIFEST_FILE;
    use crate::test_support::TempDir;

    /// 建一个最小脚本扩展目录并返回根路径。
    fn make_extension(tmp: &TempDir, id: &str, entry: &str) -> PathBuf {
        let manifest = format!(
            r#"{{
                "id": "{id}", "name": "{id}", "version": "1.0.0", "entry": "index.js"
            }}"#
        );
        tmp.write(&format!("{id}/{MANIFEST_FILE}"), &manifest);
        tmp.write(&format!("{id}/index.js"), entry);
        tmp.0.join(id)
    }

    fn registry() -> (ExtensionRegistry, Arc<ExtensionStore>) {
        let store = ExtensionStore::new(Arc::new(EmptyEnv));
        let registry = ExtensionRegistry::new(store.clone());
        (registry, store)
    }

    #[test]
    fn register_is_idempotent_and_tracks_content_changes() {
        let tmp = TempDir::new("registry-idempotent");
        let root = make_extension(&tmp, "demo", "export function go() { return 1; }");
        let (mut registry, store) = registry();

        assert_eq!(registry.register(&root).unwrap(), RegisterOutcome::Added);
        // 同一目录、内容没变：什么都不动。
        assert_eq!(
            registry.register(&root).unwrap(),
            RegisterOutcome::Unchanged
        );

        // 内容变了：Reloaded，且旧运行时已被卸载（重建交给下一次 load）。
        registry.load("demo").unwrap();
        assert!(store.is_loaded("demo"));
        std::thread::sleep(std::time::Duration::from_millis(10));
        tmp.write("demo/index.js", "export function go() { return 2; }");
        assert_eq!(registry.register(&root).unwrap(), RegisterOutcome::Reloaded);
        assert!(!store.is_loaded("demo"), "reload must drop the old runtime");
    }

    #[test]
    fn a_second_directory_with_the_same_id_is_refused() {
        let tmp = TempDir::new("registry-dup-id");
        let a = make_extension(&tmp, "demo", "export function go() {}");
        let b = tmp.0.join("demo-copy");
        // 直接写第二份清单（id 相同、目录不同）。
        tmp.write(
            "demo-copy/extension.json",
            r#"{ "id": "demo", "version": "1.0.0", "entry": "index.js" }"#,
        );
        tmp.write("demo-copy/index.js", "");
        let (mut registry, _store) = registry();

        registry.register(&a).unwrap();
        let err = registry.register(&b).unwrap_err();
        assert!(err.to_string().contains("已注册"), "{err}");
    }

    #[test]
    fn manifest_errors_are_reported_with_the_directory() {
        let tmp = TempDir::new("registry-bad-manifest");
        let root = tmp.0.join("bad");
        tmp.write("bad/extension.json", "{ not json");
        let (mut registry, _store) = registry();

        let err = registry.register(&root).unwrap_err();
        match err {
            RegisterError::Manifest { dir, message } => {
                assert!(dir.ends_with("bad"));
                assert!(message.contains("解析"), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn unregister_unloads_the_runtime_and_is_idempotent() {
        let tmp = TempDir::new("registry-unregister");
        let root = make_extension(&tmp, "demo", "export function go() {}");
        let (mut registry, store) = registry();
        registry.register(&root).unwrap();
        registry.load("demo").unwrap();
        assert!(store.is_loaded("demo"));

        assert!(registry.unregister("demo"));
        assert!(!store.is_loaded("demo"), "unregister must stop the runtime");
        assert!(!registry.unregister("demo"), "second unregister is a no-op");
        assert!(registry.get("demo").is_none());
    }

    #[test]
    fn load_is_a_noop_for_ui_only_extensions() {
        let tmp = TempDir::new("registry-ui-only");
        tmp.write(
            "panel/extension.json",
            r#"{ "id": "panel", "version": "1.0.0",
                 "commands": [{ "id": "open", "title": "面板", "ui": {} }] }"#,
        );
        tmp.write("panel/main.js", "");
        let (mut registry, store) = registry();
        registry.register(&tmp.0.join("panel")).unwrap();

        registry.load("panel").unwrap();
        assert!(
            !store.is_loaded("panel"),
            "pure UI extension needs no runtime"
        );
    }

    #[test]
    fn load_of_an_unknown_id_is_reported() {
        let (registry, _store) = registry();
        let err = registry.load("ghost").unwrap_err();
        assert!(err.to_string().contains("未注册"), "{err}");
    }
}
