//! External extensions: command bundles living next to the scripts directory.
//!
//! Two kinds share one directory level:
//!
//! - **脚本式扩展**：[`MANIFEST_FILE`] + JS/TS 入口，命令直接跑入口脚本（同普通脚本）。
//! - **UI 扩展**：[`UI_MANIFEST_FILE`] + `export default class extends View` 的入口，
//!   由 gpui-shell 在 GPUI 主线程渲染成原生窗口。
//!
//! Layout: `<data_dir>/scripter/extensions/<id>/` plus the manifest of whichever
//! kind the directory is. The two are mutually exclusive inside one directory:
//! seeing both is reported as an error rather than silently resolved.

use std::path::{Path, PathBuf};

use gpui_shell::plugin::PluginManifest;
use serde::Deserialize;

use super::script::Language;

pub const MANIFEST_FILE: &str = "extension.json";
pub const DEFAULT_ENTRY: &str = "index.js";
/// UI 扩展的清单文件名（gpui-shell 自己规定）。
pub const UI_MANIFEST_FILE: &str = gpui_shell::plugin::MANIFEST_FILE;
/// 首次启动时生成的示例 UI 扩展目录名。
pub const DEMO_UI_ID: &str = "ui-demo";

#[derive(Clone, Debug, Deserialize)]
pub struct ExtensionManifest {
    /// Must match the directory name, so command ids stay stable.
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub commands: Vec<ExtensionCommandDef>,
}

impl ExtensionManifest {
    /// Display name for the launcher group heading.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExtensionCommandDef {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
}

/// A successfully discovered extension.
#[derive(Clone, Debug)]
pub struct Extension {
    pub manifest: ExtensionManifest,
    /// Absolute path of the entry script.
    pub entry: PathBuf,
}

/// A directory that looked like an extension but failed to load.
#[derive(Clone, Debug)]
pub struct ExtensionError {
    pub dir: PathBuf,
    pub message: String,
}

/// A discovered **UI extension**: a directory whose entry describes a native
/// window through gpui-shell.
///
/// The manifest is gpui-shell's own strict type (required `id`/`name`/`entry`,
/// unknown fields rejected), so an invalid directory is reported with the same
/// message the runtime would have produced.
#[derive(Clone, Debug)]
pub struct UiExtension {
    pub manifest: PluginManifest,
    /// Absolute path of the extension directory (the gpui-shell application root).
    pub root: PathBuf,
}

impl UiExtension {
    pub fn id(&self) -> &str {
        self.manifest.id()
    }

    pub fn name(&self) -> &str {
        self.manifest.name()
    }

    /// Entry module, relative to [`Self::root`]. gpui-shell resolves it.
    pub fn entry(&self) -> &str {
        self.manifest.entry()
    }
}

/// Scan `dir` one level deep for UI extensions (directories with
/// [`UI_MANIFEST_FILE`]). One directory may not carry both manifest kinds —
/// that is reported instead of silently preferring one.
pub fn list_ui_extensions(dir: &Path) -> (Vec<UiExtension>, Vec<ExtensionError>) {
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (extensions, errors);
    };
    for entry in entries.flatten() {
        let sub = entry.path();
        if !sub.is_dir() || !sub.join(UI_MANIFEST_FILE).is_file() {
            continue;
        }
        match load_ui_extension(&sub) {
            Ok(extension) => extensions.push(extension),
            Err(message) => errors.push(ExtensionError { dir: sub, message }),
        }
    }
    extensions.sort_by(|a, b| a.id().cmp(b.id()));
    (extensions, errors)
}

fn load_ui_extension(dir: &Path) -> Result<UiExtension, String> {
    if dir.join(MANIFEST_FILE).is_file() {
        return Err(format!(
            "同时包含 {MANIFEST_FILE} 与 {UI_MANIFEST_FILE}：一个目录只能是脚本式扩展或 UI 扩展之一"
        ));
    }
    let manifest = PluginManifest::read(dir).map_err(|e| e.to_string())?;
    Ok(UiExtension {
        manifest,
        root: dir.to_path_buf(),
    })
}

/// Per-extension private data directory:
/// `~/Library/Application Support/scripter/extensions-data/<id>/`.
///
/// Kept next to `scripts/` and `extensions/` so a user editing an extension can
/// find its storage without knowing about gpui-shell's own layout.
pub fn extension_data_dir(id: &str) -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("scripter");
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

const DEMO_UI_MANIFEST: &str = r##"{
  "id": "ui-demo",
  "name": "UI 示例",
  "version": "0.1.0",
  "entry": "main.js",
  "capabilities": {
    "fs": { "read": ["${pluginDir}"] }
  }
}
"##;

const DEMO_UI_MAIN: &str = r##"// UI 扩展示例：计数器 + 输入回显。
//
// 脚本只描述界面，渲染由宿主在 GPUI 主线程完成。保存后窗口会自动热重载
// （debug 构建）。可用模块：`gpui-kit`、`gpui-base`、`gpui-fps`。
import { View, div } from "gpui-kit";
import { h_flex, v_flex, Button, Input, InputState } from "gpui-base";

export default class UiDemo extends View {
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
      .child(div().text_size(16).font_semibold().text_color(colors.foreground).child("UI 扩展示例"))
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

/// First-run convenience: write the `ui-demo` UI extension when it is absent.
/// An existing directory (even an empty one) is left untouched.
pub fn ensure_demo_ui_extension(dir: &Path) -> Result<(), String> {
    let root = dir.join(DEMO_UI_ID);
    if root.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("create {}: {e}", root.display()))?;
    std::fs::write(root.join(UI_MANIFEST_FILE), DEMO_UI_MANIFEST)
        .map_err(|e| format!("write {UI_MANIFEST_FILE}: {e}"))?;
    std::fs::write(root.join("main.js"), DEMO_UI_MAIN)
        .map_err(|e| format!("write main.js: {e}"))?;
    Ok(())
}

/// `~/Library/Application Support/scripter/extensions/`.
pub fn extensions_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("scripter");
    path.push("extensions");
    path
}

pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create extensions dir: {e}"))
}

/// Scan `dir` one level deep. Subdirectories without a manifest are ignored;
/// ones with a broken manifest are reported but do not affect the rest.
///
/// Directories that carry a UI manifest are skipped: they belong to
/// [`list_ui_extensions`], and a directory with both manifests is reported
/// there as an error.
pub fn list_extensions(dir: &Path) -> (Vec<Extension>, Vec<ExtensionError>) {
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (extensions, errors);
    };
    for entry in entries.flatten() {
        let sub = entry.path();
        if !sub.is_dir() {
            continue;
        }
        if !sub.join(MANIFEST_FILE).is_file() {
            continue;
        }
        if sub.join(UI_MANIFEST_FILE).is_file() {
            continue;
        }
        match load_extension(&sub) {
            Ok(extension) => extensions.push(extension),
            Err(message) => errors.push(ExtensionError { dir: sub, message }),
        }
    }
    extensions.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    (extensions, errors)
}

fn load_extension(dir: &Path) -> Result<Extension, String> {
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "invalid directory name".to_string())?;
    let raw = std::fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|e| format!("read {MANIFEST_FILE}: {e}"))?;
    let manifest: ExtensionManifest =
        serde_json::from_str(&raw).map_err(|e| format!("parse {MANIFEST_FILE}: {e}"))?;
    if manifest.id != dir_name {
        return Err(format!(
            "id `{}` does not match directory `{dir_name}`",
            manifest.id
        ));
    }
    let entry = dir.join(manifest.entry.as_deref().unwrap_or(DEFAULT_ENTRY));
    if !entry.is_file() {
        return Err(format!("entry file missing: {}", entry.display()));
    }
    if Language::from_path(&entry).is_none() {
        return Err(format!("entry must be .js or .ts: {}", entry.display()));
    }
    Ok(Extension { manifest, entry })
}

/// The commands an extension contributes. A manifest without an explicit
/// `commands` array still gets one default command that runs the entry.
pub fn commands_of(extension: &Extension) -> Vec<ExtensionCommandDef> {
    if extension.manifest.commands.is_empty() {
        return vec![ExtensionCommandDef {
            id: "run".to_string(),
            title: extension.manifest.display_name().to_string(),
            subtitle: extension.manifest.description.clone(),
        }];
    }
    extension.manifest.commands.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn missing_dir_yields_nothing() {
        let (exts, errors) = list_extensions(Path::new("/nonexistent/scripter-test"));
        assert!(exts.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn loads_valid_extension_with_defaults() {
        let tmp = TempDir::new("valid");
        tmp.write(
            "demo/extension.json",
            r#"{ "id": "demo", "name": "Demo", "description": "test ext" }"#,
        );
        tmp.write("demo/index.js", "console.log('hi');\n");

        let (exts, errors) = list_extensions(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(exts.len(), 1);
        let ext = &exts[0];
        assert_eq!(ext.manifest.id, "demo");
        assert_eq!(ext.manifest.display_name(), "Demo");
        assert!(ext.entry.ends_with("index.js"));
        // No explicit commands -> one synthesized default command.
        let commands = commands_of(ext);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].title, "Demo");
    }

    #[test]
    fn loads_typescript_entry_and_commands() {
        let tmp = TempDir::new("ts-entry");
        tmp.write(
            "tools/extension.json",
            r#"{
                "id": "tools",
                "entry": "main.ts",
                "commands": [
                    { "id": "a", "title": "Do A", "subtitle": "sub" },
                    { "id": "b", "title": "Do B" }
                ]
            }"#,
        );
        tmp.write("tools/main.ts", "console.log('ts');\n");

        let (exts, errors) = list_extensions(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(exts.len(), 1);
        assert!(exts[0].entry.ends_with("main.ts"));
        let commands = commands_of(&exts[0]);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].id, "a");
        assert_eq!(commands[1].subtitle, None);
    }

    #[test]
    fn id_mismatch_is_an_error() {
        let tmp = TempDir::new("mismatch");
        tmp.write("foo/extension.json", r#"{ "id": "bar" }"#);
        tmp.write("foo/index.js", "");

        let (exts, errors) = list_extensions(&tmp.0);
        assert!(exts.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("does not match"));
    }

    #[test]
    fn broken_manifest_does_not_block_others() {
        let tmp = TempDir::new("mixed");
        tmp.write("bad/extension.json", "{ not json");
        tmp.write("good/extension.json", r#"{ "id": "good" }"#);
        tmp.write("good/index.js", "");
        // A directory without a manifest is ignored, not an error.
        tmp.write("random/readme.txt", "hello");

        let (exts, errors) = list_extensions(&tmp.0);
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].manifest.id, "good");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn missing_entry_is_an_error() {
        let tmp = TempDir::new("no-entry");
        tmp.write("demo/extension.json", r#"{ "id": "demo" }"#);

        let (exts, errors) = list_extensions(&tmp.0);
        assert!(exts.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("entry file missing"));
    }

    #[test]
    fn non_script_entry_is_an_error() {
        let tmp = TempDir::new("bad-entry");
        tmp.write(
            "demo/extension.json",
            r#"{ "id": "demo", "entry": "run.py" }"#,
        );
        tmp.write("demo/run.py", "print('no')");

        let (exts, errors) = list_extensions(&tmp.0);
        assert!(exts.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("must be .js or .ts"));
    }

    // ── UI 扩展 ────────────────────────────────────────────────────────────

    const UI_MANIFEST: &str = r#"{ "id": "ui", "name": "UI 示例", "entry": "main.js" }"#;

    #[test]
    fn loads_ui_extension_and_keeps_it_out_of_script_extensions() {
        let tmp = TempDir::new("ui-load");
        tmp.write("ui/gpui-shell.json", UI_MANIFEST);
        tmp.write("ui/main.js", "export default class extends View {}");
        // 脚本式扩展与 UI 扩展共存。
        tmp.write("tools/extension.json", r#"{ "id": "tools" }"#);
        tmp.write("tools/index.js", "");

        let (ui, ui_errors) = list_ui_extensions(&tmp.0);
        assert!(ui_errors.is_empty());
        assert_eq!(ui.len(), 1);
        assert_eq!(ui[0].id(), "ui");
        assert_eq!(ui[0].name(), "UI 示例");
        assert_eq!(ui[0].entry(), "main.js");
        assert!(ui[0].root.ends_with("ui"));

        let (scripts, script_errors) = list_extensions(&tmp.0);
        assert!(script_errors.is_empty());
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].manifest.id, "tools");
    }

    #[test]
    fn broken_ui_manifest_is_reported_without_blocking_others() {
        let tmp = TempDir::new("ui-broken");
        // 缺少必需的 name / entry。
        tmp.write("bad/gpui-shell.json", r#"{ "id": "bad" }"#);
        tmp.write("good/gpui-shell.json", UI_MANIFEST);
        tmp.write("good/main.js", "");

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert_eq!(ui.len(), 1);
        assert_eq!(ui[0].id(), "ui");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].dir.ends_with("bad"));
        assert!(!errors[0].message.is_empty());
    }

    #[test]
    fn both_manifests_in_one_directory_is_an_error() {
        let tmp = TempDir::new("ui-both");
        tmp.write("mixed/extension.json", r#"{ "id": "mixed" }"#);
        tmp.write("mixed/index.js", "");
        tmp.write("mixed/gpui-shell.json", UI_MANIFEST);
        tmp.write("mixed/main.js", "");

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert!(ui.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("gpui-shell.json"));

        // 脚本式发现同样跳过它，不能两边都列一遍。
        let (scripts, _) = list_extensions(&tmp.0);
        assert!(scripts.is_empty());
    }

    #[test]
    fn demo_ui_extension_is_written_once() {
        let tmp = TempDir::new("ui-demo");
        ensure_demo_ui_extension(&tmp.0).expect("demo should be created");
        let manifest = tmp.0.join(DEMO_UI_ID).join(UI_MANIFEST_FILE);
        assert!(manifest.is_file());

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(ui.len(), 1);
        assert_eq!(ui[0].id(), DEMO_UI_ID);

        // 已存在时不再覆盖作者改动。
        std::fs::write(&manifest, UI_MANIFEST).expect("rewrite manifest");
        ensure_demo_ui_extension(&tmp.0).expect("second call should be a no-op");
        assert_eq!(
            std::fs::read_to_string(&manifest).expect("read manifest"),
            UI_MANIFEST
        );
    }

    #[test]
    fn extension_data_dir_lives_under_the_app_data_dir() {
        let dir = extension_data_dir("ui-demo");
        assert!(dir.ends_with("scripter/extensions-data/ui-demo"));
    }
}
