//! 扫描标准目录里的 `.app`。
//!
//! 只做文件系统遍历（不碰 AppKit），因此可以同步跑在启动路径上：
//! 三个根目录、几百个条目，耗时只有几十毫秒。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 一个可启动的应用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppEntry {
    /// 规范化后的 `.app` 路径（去重与图标缓存都用它作键）。
    pub(crate) path: PathBuf,
    /// 显示名：`Safari.app` → `Safari`。
    pub(crate) name: String,
    /// 副标题：所在根目录的说明，或根下的相对目录（如 `应用 / Utilities`）。
    pub(crate) subtitle: String,
}

/// 一个扫描根及其说明。
pub(crate) struct ScanRoot {
    pub(crate) path: PathBuf,
    pub(crate) label: &'static str,
}

/// 默认扫描三处标准目录。
pub(crate) fn default_roots() -> Vec<ScanRoot> {
    let mut roots = vec![
        ScanRoot {
            path: PathBuf::from("/Applications"),
            label: "应用",
        },
        ScanRoot {
            path: PathBuf::from("/System/Applications"),
            label: "系统应用",
        },
    ];
    if let Some(home) = dirs::home_dir() {
        roots.push(ScanRoot {
            path: home.join("Applications"),
            label: "用户应用",
        });
    }
    roots
}

/// 递归扫描所有根目录；不进入 `.app` 内部，跳过隐藏目录与符号链接目录
/// （防止软链成环）。结果按名称不区分大小写排序。
pub(crate) fn scan_apps(roots: &[ScanRoot]) -> Vec<AppEntry> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for root in roots {
        collect(&root.path, &root.path, root.label, &mut seen, &mut apps);
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn collect(
    dir: &Path,
    root: &Path,
    label: &'static str,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<AppEntry>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if name.ends_with(".app") {
            // canonicalize：同一个 App 可能出现在多个根（或经软链），去重。
            let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(canonical.clone()) {
                continue;
            }
            out.push(AppEntry {
                name: name.trim_end_matches(".app").to_string(),
                // 副标题用原始路径算：canonicalize 会把 /var 解析成 /private/var，
                // 与扫描根的字符串前缀对不上。
                subtitle: subtitle_for(&path, root, label),
                path: canonical,
            });
            // .app 是叶子：绝不进入 Contents 内部找嵌套的 .app。
            continue;
        }
        collect(&path, root, label, seen, out);
    }
}

fn subtitle_for(path: &Path, root: &Path, label: &str) -> String {
    let parent = path.parent().unwrap_or(root);
    let relative = parent.strip_prefix(root).unwrap_or(Path::new(""));
    if relative.as_os_str().is_empty() {
        label.to_string()
    } else {
        format!("{label} / {}", relative.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn touch_app(dir: &Path, name: &str) -> PathBuf {
        let app = dir.join(name);
        std::fs::create_dir_all(&app).unwrap();
        app
    }

    #[test]
    fn scans_apps_recursively_and_skips_non_apps() {
        let temp = TempDir::new("apps-scan");
        let root = temp.0.join("Applications");
        touch_app(&root, "Safari.app");
        touch_app(&root, "Notes.app");
        let utilities = root.join("Utilities");
        std::fs::create_dir_all(&utilities).unwrap();
        touch_app(&utilities, "Terminal.app");
        // 非 .app 目录与文件都不应被收进来。
        std::fs::create_dir_all(root.join("Support")).unwrap();
        std::fs::write(root.join("readme.txt"), b"x").unwrap();
        // 隐藏目录整棵跳过。
        touch_app(&root.join(".hidden"), "Ghost.app");

        let apps = scan_apps(&[ScanRoot {
            path: root.clone(),
            label: "应用",
        }]);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Notes", "Safari", "Terminal"]);

        let terminal = apps.iter().find(|a| a.name == "Terminal").unwrap();
        assert_eq!(terminal.subtitle, "应用 / Utilities");
        let safari = apps.iter().find(|a| a.name == "Safari").unwrap();
        assert_eq!(safari.subtitle, "应用");
        // `.app` 是叶子：Contents 里再放一个 .app 也不会被扫描。
        touch_app(&root.join("Safari.app").join("Contents"), "Inner.app");
        let apps = scan_apps(&[ScanRoot {
            path: root,
            label: "应用",
        }]);
        assert_eq!(apps.len(), 3);
        assert!(!apps.iter().any(|a| a.name == "Inner"));
    }

    #[test]
    fn dedupes_the_same_app_across_roots() {
        let temp = TempDir::new("apps-dedupe");
        let root = temp.0.join("Applications");
        touch_app(&root, "Safari.app");
        let apps = scan_apps(&[
            ScanRoot {
                path: root.clone(),
                label: "应用",
            },
            ScanRoot {
                path: root,
                label: "系统应用",
            },
        ]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Safari");
    }
}
