//! 扩展目录指纹：判断「同一 id」的目录内容有没有被改过。
//!
//! 注册表对已存在的 id 只在内容变化时才重新登记（见
//! [`crate::registry::ExtensionRegistry::register`]）；而开发期最常见的动作恰恰
//! 是「改完代码就期待生效」——磁盘变了，注册表里的元数据和跑着的引擎却都停在
//! 旧内容上。指纹就是发现这件事的依据。
//!
//! 取 `(相对路径, 大小, mtime)` 的哈希而不是文件内容哈希：扩展目录里可能躺着
//! 整包前端资源，逐字节读会把"打开一次 Launcher"变成一次全目录 IO。代价是
//! 「大小不变、且 mtime 落在同一时间粒度内」的改动识别不到；编辑器保存一次就
//! 会刷新 mtime，实际不会踩到。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// 不参与指纹的目录：依赖包与版本库的变动跟扩展代码无关。
const SKIPPED_DIRS: &[&str] = &["node_modules", ".git"];

/// 遍历上限。扩展目录正常只有几十个文件；真有人塞了几千个，也没必要为了
/// 指纹把目录完整走一遍。
const MAX_ENTRIES: usize = 4096;

/// 一个目录的内容指纹；目录读不了时返回 `None`（宿主据此"不判断"，
/// 而不是误判成"变过了"）。
pub fn dir_fingerprint(root: &Path) -> Option<u64> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    // 目录遍历顺序在不同文件系统上不保证一致：排序后哈希才有可比性。
    files.sort();
    Some(hash(&files))
}

/// 单文件指纹，给只看入口的场景留的口子（当前宿主用整目录）。
pub fn file_fingerprint(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let mut hasher = DefaultHasher::new();
    path.file_name()?.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    modified_nanos(&meta).hash(&mut hasher);
    Some(hasher.finish())
}

/// 收集 `root` 下的普通文件：`(相对路径, 大小, mtime 纳秒)`。
fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, u64, u128)>) -> Option<()> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(meta) = entry.metadata() else { continue };

        // 符号链接不跟随：指向目录时可能成环，指向外面时内容又不属于本扩展。
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if SKIPPED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk(root, &path, out);
            continue;
        }
        // 声明文件是工具生成的（`gpui-kit.d.ts`），重生成不该让扩展重载。
        if name.ends_with(".d.ts") || !meta.is_file() {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        out.push((relative, meta.len(), modified_nanos(&meta)));
    }
    Some(())
}

fn modified_nanos(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or(0)
}

fn hash(files: &[(PathBuf, u64, u128)]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for (path, len, modified) in files {
        path.hash(&mut hasher);
        len.hash(&mut hasher);
        modified.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn extension_dir(tmp: &TempDir) -> PathBuf {
        let root = tmp.0.join("demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("extension.json"), "{}").unwrap();
        std::fs::write(root.join("index.js"), "export function a() {}").unwrap();
        root
    }

    #[test]
    fn same_content_gives_the_same_fingerprint() {
        let tmp = TempDir::new("fingerprint-stable");
        let root = extension_dir(&tmp);
        let first = dir_fingerprint(&root).expect("readable dir");
        let second = dir_fingerprint(&root).expect("readable dir");
        assert_eq!(first, second);
    }

    #[test]
    fn editing_a_file_changes_the_fingerprint() {
        let tmp = TempDir::new("fingerprint-edit");
        let root = extension_dir(&tmp);
        let before = dir_fingerprint(&root).unwrap();

        std::fs::write(root.join("index.js"), "export function a() { return 1; }").unwrap();
        let after = dir_fingerprint(&root).unwrap();
        assert_ne!(before, after);
    }

    /// 开发期最典型的一次编辑：改个字符串字面量，文件大小可能一样。
    #[test]
    fn same_size_edit_is_still_detected() {
        let tmp = TempDir::new("fingerprint-samesize");
        let root = extension_dir(&tmp);
        std::fs::write(root.join("index.js"), "return 'aaaaaaaa'").unwrap();
        let before = dir_fingerprint(&root).unwrap();

        // 等 mtime 走一格：同一纳秒内的两次写，任何基于时间戳的实现都分不出来。
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("index.js"), "return 'bbbbbbbb'").unwrap();

        assert_ne!(before, dir_fingerprint(&root).unwrap());
    }

    #[test]
    fn adding_a_file_changes_the_fingerprint() {
        let tmp = TempDir::new("fingerprint-add");
        let root = extension_dir(&tmp);
        let before = dir_fingerprint(&root).unwrap();

        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("lib/helper.js"), "export const x = 1;").unwrap();
        assert_ne!(before, dir_fingerprint(&root).unwrap());
    }

    #[test]
    fn generated_declarations_and_dependency_dirs_are_ignored() {
        let tmp = TempDir::new("fingerprint-ignored");
        let root = extension_dir(&tmp);
        let before = dir_fingerprint(&root).unwrap();

        std::fs::write(root.join("gpui-kit.d.ts"), "declare const x: number;").unwrap();
        std::fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        std::fs::write(root.join("node_modules/dep/index.js"), "module.exports = 1").unwrap();
        assert_eq!(before, dir_fingerprint(&root).unwrap());
    }

    #[test]
    fn missing_directory_has_no_fingerprint() {
        let tmp = TempDir::new("fingerprint-missing");
        assert_eq!(dir_fingerprint(&tmp.0.join("nope")), None);
    }
}
