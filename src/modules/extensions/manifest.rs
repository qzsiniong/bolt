//! 扩展目录的定位、播种与扫描。
//!
//! 目录约定（plux 的 bundle 命名）：`<extensions>/<id>-v<version>.js/`，
//! 目录里放 `extension.json` 与入口脚本。清单本身的解析与校验在
//! `plux_js_manager::manifest` 里，本模块只管"文件在哪、怎么播种、哪些目录
//! 看起来是扩展但已经不可用"。
//!
//! 没有 `extension.json` 的目录一律忽略——作者的草稿不该刷一屏错误。

use std::path::{Path, PathBuf};

pub use plux_js_manager::{
    EXTENSION_SCHEMA, MANIFEST_FILE, SCHEMA_FILE, UI_ENTRY, UiActionKind, UiBuiltin,
};

/// gpui-shell 自己的清单名。同时存在会让 gpui-shell 走严格清单路径，故禁止。
pub const SHELL_MANIFEST_FILE: &str = "gpui-shell.json";

/// 首次启动时落盘的示例扩展目录（脚本命令 + UI 命令 + 标题栏动作）。
///
/// 内容的唯一事实来源是仓库的 `extensions/demo-v1.0.0.js/`：用 `include_str!`
/// 在编译期嵌进来，release 二进制找不到仓库也能写出同一样的一份；改示例后
/// 重编译即同步。仓库形态（debug 构建）不落盘——仓库目录本身就是扩展目录。
pub const DEMO_DIR: &str = "demo-v1.0.0.js";

pub(super) const DEMO_MANIFEST: &str =
    include_str!("../../../extensions/demo-v1.0.0.js/extension.json");

/// 示例扩展的全部文件：`(扩展目录内的相对路径, 内容)`。
const DEMO_FILES: &[(&str, &str)] = &[
    (MANIFEST_FILE, DEMO_MANIFEST),
    (
        "index.js",
        include_str!("../../../extensions/demo-v1.0.0.js/index.js"),
    ),
    (
        UI_ENTRY,
        include_str!("../../../extensions/demo-v1.0.0.js/main.js"),
    ),
    (
        "jsconfig.json",
        include_str!("../../../extensions/demo-v1.0.0.js/jsconfig.json"),
    ),
    (
        "icons/star.svg",
        include_str!("../../../extensions/demo-v1.0.0.js/icons/star.svg"),
    ),
];

/// `~/Library/Application Support/bolt/extensions/`。
pub fn extensions_dir() -> PathBuf {
    bolt_model::paths::data_root().join("extensions")
}

/// 仓库内随源码维护的扩展目录（开发副本）。
///
/// 先看当前工作目录（`cargo run` 从仓库根启动），再回退编译期包根（直接运行
/// `target/debug/bolt` 时 CWD 未必在仓库里）。
pub fn bundled_extensions_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    cwd.as_deref()
        .and_then(extensions_under)
        .or_else(|| extensions_under(Path::new(env!("CARGO_MANIFEST_DIR"))))
}

/// `base/extensions/` 若存在则返回它。
fn extensions_under(base: &Path) -> Option<PathBuf> {
    let candidate = base.join("extensions");
    candidate.is_dir().then_some(candidate)
}

/// [`active_extensions_dir`] 是否直接落在仓库目录里（即 debug 开发形态）。
pub fn uses_bundled_extensions() -> bool {
    cfg!(debug_assertions) && bundled_extensions_dir().is_some()
}

/// 实际生效的扩展目录。
///
/// debug 构建下只要找得到仓库 `extensions/` 就直接用它：改仓库里的扩展文件
/// 重启即生效，省掉「先同步一份到数据目录」这一步。release 构建、或找不到
/// 仓库目录时用数据目录。
pub fn active_extensions_dir() -> PathBuf {
    let bundled = if cfg!(debug_assertions) {
        bundled_extensions_dir()
    } else {
        None
    };
    bundled.unwrap_or_else(extensions_dir)
}

/// 确保目录存在（不存在则递归创建）。
pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))
}

/// 把清单的 JSON Schema 摆在扩展根目录（与扩展目录同级）。
///
/// 放根上而不是每个扩展里：所有扩展的清单都用同一个相对路径
/// `../extension.schema.json` 引用它，schema 升级时也就只有这一份要换。宿主
/// 每次启动重写（内容没变则不动），所以 user 目录里的副本不会停在旧版本。
///
/// 它只是个编辑器提示文件：`scan` 只看目录，扩展加载完全不读它。
pub fn ensure_schema(dir: &Path) -> Result<(), String> {
    crate::model::project_gen::write_if_changed(&dir.join(SCHEMA_FILE), EXTENSION_SCHEMA)
}

/// 扫描扩展根，返回可注册的扩展目录与错误消息。
///
/// - 有 `extension.json` → 收集（是否合法交给 plux manager 判断）。
/// - 没有 → 忽略（作者的草稿目录不该刷一屏错误）。
/// - 同时有 `gpui-shell.json` → 报错，因为 gpui-shell 会走严格清单路径。
pub fn scan(extensions: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();

    for entry in read_dirs(extensions) {
        inspect(&entry, &mut found, &mut errors);
    }

    found.sort();
    (found, errors)
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
        })
        .collect()
}

fn inspect(dir: &Path, found: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    if !dir.join(MANIFEST_FILE).is_file() {
        return;
    }
    if dir.join(SHELL_MANIFEST_FILE).is_file() {
        errors.push(format!(
            "扩展加载失败 {}：请删除 {SHELL_MANIFEST_FILE}（UI 能力由宿主安装，清单只需 {MANIFEST_FILE}）",
            dir.display()
        ));
        return;
    }
    found.push(dir.to_path_buf());
}

/// 把仓库 `extensions/` 下的扩展目录播种到数据目录，返回复制个数。
/// 已存在的目录一律不动：数据目录里的内容是用户的，不覆盖。
pub fn ensure_bundled_extensions(target: &Path) -> Result<usize, String> {
    match bundled_extensions_dir() {
        Some(source) => sync_extensions(&source, target),
        None => Ok(0),
    }
}

fn sync_extensions(source: &Path, target: &Path) -> Result<usize, String> {
    ensure_dir(target)?;
    let entries = std::fs::read_dir(source)
        .map_err(|e| format!("read bundled extensions {}: {e}", source.display()))?;
    let mut count = 0;
    for entry in entries.flatten() {
        let src = entry.path();
        // 只播种看起来是扩展的目录：有清单才算，草稿与杂项留在仓库里。
        if !src.is_dir() || !src.join(MANIFEST_FILE).is_file() {
            continue;
        }
        let dst = target.join(entry.file_name());
        if dst.exists() {
            continue;
        }
        copy_tree(&src, &dst)?;
        count += 1;
    }
    Ok(count)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("create {}: {e}", dst.display()))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// 每个扩展的私有数据目录：
/// `~/Library/Application Support/bolt/extensions-data/<id>/`。
pub fn extension_data_dir(id: &str) -> PathBuf {
    bolt_model::paths::data_root()
        .join("extensions-data")
        .join(id)
}

/// 确保扩展的私有数据目录存在并返回其路径。
pub fn ensure_extension_data_dir(id: &str) -> Result<PathBuf, String> {
    let dir = extension_data_dir(id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create extension data dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// 首次启动的便利措施：示例扩展目录不存在时写出内嵌的 demo 扩展。
/// 已存在的目录一律不动（作者的改动不该被覆盖）。
pub fn ensure_demo_extension(extensions: &Path) -> Result<(), String> {
    let root = extensions.join(DEMO_DIR);
    if root.exists() {
        return Ok(());
    }
    for (relative, content) in DEMO_FILES {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, content).map_err(|e| format!("write {relative}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn missing_dir_yields_nothing() {
        let tmp = TempDir::new("ext-scan-missing");
        let (extensions, errors) = scan(&tmp.0.join("nope"));
        assert!(extensions.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn scans_extension_dirs_and_skips_drafts() {
        let tmp = TempDir::new("ext-scan-ok");
        tmp.write(&format!("demo-v1.0.0.js/{MANIFEST_FILE}"), "{}");
        tmp.write("scratch/notes.txt", "draft");

        let (extensions, errors) = scan(&tmp.0);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(extensions.len(), 1);
        assert!(extensions[0].ends_with(DEMO_DIR));
    }

    #[test]
    fn gpui_shell_manifest_is_rejected() {
        let tmp = TempDir::new("ext-shell-manifest");
        tmp.write(&format!("demo-v1.0.0.js/{MANIFEST_FILE}"), "{}");
        tmp.write(&format!("demo-v1.0.0.js/{SHELL_MANIFEST_FILE}"), "{}");

        let (extensions, errors) = scan(&tmp.0);
        assert!(extensions.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(SHELL_MANIFEST_FILE), "{errors:?}");
    }

    #[test]
    fn demo_extension_is_written_once() {
        let tmp = TempDir::new("ext-demo");
        ensure_demo_extension(&tmp.0).expect("demo should be created");
        // 每一个内嵌的文件都要落盘，包括子目录（icons/star.svg）。
        for (relative, content) in DEMO_FILES {
            let path = tmp.0.join(DEMO_DIR).join(relative);
            assert!(path.is_file(), "{relative} 应该被写出来");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                *content,
                "{relative} 的内容应与内嵌副本一致"
            );
        }

        let manifest = tmp.0.join(DEMO_DIR).join(MANIFEST_FILE);
        let custom = r#"{ "id": "demo", "version": "1.0.0" }"#;
        std::fs::write(&manifest, custom).unwrap();
        ensure_demo_extension(&tmp.0).expect("second call should be a no-op");
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), custom);
    }

    #[test]
    fn bundled_extension_copy_only_missing() {
        let source = TempDir::new("ext-bundled-src");
        source.write(&format!("a-v1.0.0.js/{MANIFEST_FILE}"), "{}");
        source.write("a-v1.0.0.js/index.js", "export function run() {}");
        source.write(&format!("b-v1.0.0.js/{MANIFEST_FILE}"), "{}");

        let target = TempDir::new("ext-bundled-dst");
        let copied = sync_extensions(&source.0, &target.0).expect("first sync");
        assert_eq!(copied, 2);
        assert!(target.0.join("a-v1.0.0.js/index.js").is_file());

        std::fs::write(
            target.0.join("a-v1.0.0.js/index.js"),
            "export function run() { /* local */ }",
        )
        .unwrap();
        let copied = sync_extensions(&source.0, &target.0).expect("second sync");
        assert_eq!(copied, 0);
        assert!(
            std::fs::read_to_string(target.0.join("a-v1.0.0.js/index.js"))
                .unwrap()
                .contains("local")
        );
    }

    #[test]
    fn extension_data_dir_lives_under_the_app_data_dir() {
        assert!(extension_data_dir("demo").ends_with("bolt/extensions-data/demo"));
    }

    #[test]
    fn active_dir_prefers_bundled_in_debug() {
        match bundled_extensions_dir() {
            Some(bundled) => {
                assert!(uses_bundled_extensions());
                assert_eq!(active_extensions_dir(), bundled);
            }
            None => {
                assert!(!uses_bundled_extensions());
                assert_eq!(active_extensions_dir(), extensions_dir());
            }
        }
    }
}
