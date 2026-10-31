//! 插件目录的定位、播种与扫描。
//!
//! 目录约定（plux 的 bundle 命名）：`<extensions>/<id>-v<version>.js/`，
//! 目录里放 `extension.json` 与入口脚本。清单本身的解析与校验在
//! `plux_js_manager::manifest` 里，本模块只管"文件在哪、怎么播种、哪些目录
//! 看起来是插件但已经不可用"。
//!
//! 没有 `extension.json` 的目录一律忽略——作者的草稿不该刷一屏错误。

use std::path::{Path, PathBuf};

pub use plux_js_manager::{
    EXTENSION_SCHEMA, MANIFEST_FILE, SCHEMA_FILE, UI_ENTRY, UiActionKind, UiBuiltin,
};

/// gpui-shell 自己的清单名。同时存在会让 gpui-shell 走严格清单路径，故禁止。
pub const SHELL_MANIFEST_FILE: &str = "gpui-shell.json";

/// 首次启动时生成的示例插件目录（脚本命令 + UI 命令各一条）。
pub const DEMO_DIR: &str = "demo-v1.0.0.js";

/// `~/Library/Application Support/bolt/extensions/`。
pub fn extensions_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("extensions");
    path
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
/// debug 构建下只要找得到仓库 `extensions/` 就直接用它：改仓库里的插件文件
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

pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))
}

/// 把清单的 JSON Schema 摆在扩展根目录（与插件目录同级）。
///
/// 放根上而不是每个插件里：所有插件的清单都用同一个相对路径
/// `../extension.schema.json` 引用它，schema 升级时也就只有这一份要换。宿主
/// 每次启动重写（内容没变则不动），所以 user 目录里的副本不会停在旧版本。
///
/// 它只是个编辑器提示文件：`scan` 只看目录，插件加载完全不读它。
pub fn ensure_schema(dir: &Path) -> Result<(), String> {
    crate::model::project_gen::write_if_changed(&dir.join(SCHEMA_FILE), EXTENSION_SCHEMA)
}

/// 扫描扩展根，返回可注册的插件目录与错误消息。
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

/// 把仓库 `extensions/` 下的插件目录播种到数据目录，返回复制个数。
/// 已存在的目录一律不动：数据目录里的内容是用户的，不覆盖。
pub fn ensure_bundled_extensions(target: &Path) -> Result<usize, String> {
    match bundled_extensions_dir() {
        Some(source) => sync_plugins(&source, target),
        None => Ok(0),
    }
}

fn sync_plugins(source: &Path, target: &Path) -> Result<usize, String> {
    ensure_dir(target)?;
    let entries = std::fs::read_dir(source)
        .map_err(|e| format!("read bundled plugins {}: {e}", source.display()))?;
    let mut count = 0;
    for entry in entries.flatten() {
        let src = entry.path();
        // 只播种看起来是插件的目录：有清单才算，草稿与杂项留在仓库里。
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

/// Per-extension private data directory:
/// `~/Library/Application Support/bolt/extensions-data/<id>/`。
pub fn extension_data_dir(id: &str) -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("extensions-data");
    path.push(id);
    path
}

pub fn ensure_extension_data_dir(id: &str) -> Result<PathBuf, String> {
    let dir = extension_data_dir(id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create extension data dir {}: {e}", dir.display()))?;
    Ok(dir)
}

pub(super) const DEMO_MANIFEST: &str = r#"{
  "$schema": "../extension.schema.json",
  "id": "demo",
  "name": "演示插件",
  "description": "同时演示脚本命令与 UI 命令",
  "version": "1.0.0",
  "entry": "index.js",
  "commands": [
    { "id": "hello", "title": "打招呼", "description": "在 Console 里打印一行问候" },
    {
      "id": "panel",
      "title": "打开面板",
      "description": "在原生窗口里渲染一个计数器",
      "ui": { "title": "演示面板", "width": 480, "height": 320 }
    }
  ]
}
"#;

const DEMO_INDEX: &str = r#"// 脚本命令示例。
//
// 命令 id 就是入口模块里要导出的函数名；宿主按
// 「具名导出 → `export default` 对象 → `globalThis.plux.commands`」三档查找。
// 每条命令都在插件自己的常驻运行时里执行，模块级状态会保留到下次调用。
export function hello(ctx) {
  const { id, version } = ctx.plugin;
  return `你好，这是 ${id} v${version} 的 ${ctx.command} 命令`;
}
"#;

const DEMO_MAIN: &str = r##"// UI 命令示例：脚本只描述界面，渲染由宿主在 GPUI 主线程完成。
//
// 保存后窗口会自动热重载（debug 构建）。可用模块：`gpui-kit`、`gpui-base`、
// `gpui-fps`。窗口置顶由宿主标题栏上的图钉控制（清单里的 always_on_top 只是
// 初始状态）。
import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Input, InputState } from "gpui-base";

export default class DemoPanel extends View {
  init() {
    this.count = 0;
    this.draft = InputState.new({ placeholder: "输入点什么…" });
    this.draft.on("submit", (_event, cx) => this.echo(cx));
  }

  /// 把输入框内容弹成 toast（shell 自带的浮层，宿主窗口根是 ShellRoot 时可用）。
  echo(cx) {
    const text = this.draft.value().trim();
    if (text === "") return;
    window.push_toast({ title: `你输入了：${text}`, level: "info", id: "echo" });
    this.draft.set_value("");
    cx.notify();
  }

  render(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .p(24)
      .gap(12)
      .bg(colors.background)
      .child(div().text_size(16).font_semibold().text_color(colors.foreground).child("演示面板"))
      .child(
        div()
          .text_size(12)
          .text_color(colors.muted_foreground)
          .child(`当前计数：${this.count}`),
      )
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .child(this.button("inc", "加一", cx, () => (this.count += 1)))
          .child(this.button("reset", "归零", cx, () => (this.count = 0))),
      )
      .child(
        h_flex()
          .gap(8)
          .items_center()
          .child(Input.new(this.draft).flex_1().h(28).px(8).border(1).border_color(colors.input).bg(colors.surface).text_size(12))
          .child(this.button("echo", "回显", cx, (cx) => this.echo(cx))),
      );
  }

  /// base 的 Button 不带任何视觉样式，样式由脚本写。
  button(id, caption, cx, run) {
    const colors = cx.theme().colors;
    return Button.new(id)
      .flex()
      .items_center()
      .justify_center()
      .h(28)
      .px(12)
      .bg(colors.surface)
      .text_color(colors.foreground)
      .border(1)
      .border_color(colors.border)
      .rounded(6)
      .hover((style) => style.bg(colors.muted))
      .on_click((_event, cx) => {
        run(cx);
        cx.notify();
      })
      .child(caption);
  }
}
"##;

/// First-run convenience: write the `demo` plugin when its directory is absent.
/// An existing directory is left untouched (作者的改动不该被覆盖).
pub fn ensure_demo_plugin(extensions: &Path) -> Result<(), String> {
    let root = extensions.join(DEMO_DIR);
    if root.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
    std::fs::write(root.join(MANIFEST_FILE), DEMO_MANIFEST)
        .map_err(|e| format!("write {MANIFEST_FILE}: {e}"))?;
    std::fs::write(root.join("index.js"), DEMO_INDEX)
        .map_err(|e| format!("write index.js: {e}"))?;
    std::fs::write(root.join(UI_ENTRY), DEMO_MAIN).map_err(|e| format!("write {UI_ENTRY}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn missing_dir_yields_nothing() {
        let tmp = TempDir::new("ext-scan-missing");
        let (plugins, errors) = scan(&tmp.0.join("nope"));
        assert!(plugins.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn scans_plugin_dirs_and_skips_drafts() {
        let tmp = TempDir::new("ext-scan-ok");
        tmp.write(&format!("demo-v1.0.0.js/{MANIFEST_FILE}"), "{}");
        tmp.write("scratch/notes.txt", "draft");

        let (plugins, errors) = scan(&tmp.0);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(plugins.len(), 1);
        assert!(plugins[0].ends_with(DEMO_DIR));
    }

    #[test]
    fn gpui_shell_manifest_is_rejected() {
        let tmp = TempDir::new("ext-shell-manifest");
        tmp.write(&format!("demo-v1.0.0.js/{MANIFEST_FILE}"), "{}");
        tmp.write(&format!("demo-v1.0.0.js/{SHELL_MANIFEST_FILE}"), "{}");

        let (plugins, errors) = scan(&tmp.0);
        assert!(plugins.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(SHELL_MANIFEST_FILE), "{errors:?}");
    }

    #[test]
    fn demo_plugin_is_written_once() {
        let tmp = TempDir::new("ext-demo");
        ensure_demo_plugin(&tmp.0).expect("demo should be created");
        let manifest = tmp.0.join(DEMO_DIR).join(MANIFEST_FILE);
        assert!(manifest.is_file());
        assert!(tmp.0.join(DEMO_DIR).join("index.js").is_file());
        assert!(tmp.0.join(DEMO_DIR).join(UI_ENTRY).is_file());

        let custom = r#"{ "id": "demo", "version": "1.0.0" }"#;
        std::fs::write(&manifest, custom).unwrap();
        ensure_demo_plugin(&tmp.0).expect("second call should be a no-op");
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), custom);
    }

    #[test]
    fn bundled_plugins_copy_only_missing() {
        let source = TempDir::new("ext-bundled-src");
        source.write(&format!("a-v1.0.0.js/{MANIFEST_FILE}"), "{}");
        source.write("a-v1.0.0.js/index.js", "export function run() {}");
        source.write(&format!("b-v1.0.0.js/{MANIFEST_FILE}"), "{}");

        let target = TempDir::new("ext-bundled-dst");
        let copied = sync_plugins(&source.0, &target.0).expect("first sync");
        assert_eq!(copied, 2);
        assert!(target.0.join("a-v1.0.0.js/index.js").is_file());

        std::fs::write(
            target.0.join("a-v1.0.0.js/index.js"),
            "export function run() { /* local */ }",
        )
        .unwrap();
        let copied = sync_plugins(&source.0, &target.0).expect("second sync");
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
