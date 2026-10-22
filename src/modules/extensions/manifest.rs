//! External extensions: command bundles living next to the scripts directory.
//!
//! One manifest name ([`MANIFEST_FILE`]) describes both kinds; the `kind` field
//! tells them apart:
//!
//! - **脚本式扩展**（`kind` 省略或 `"script"`）：JS/TS 入口，命令直接跑入口脚本（同普通脚本）。
//! - **UI 扩展**（`"kind": "ui"`）：固定入口 [`UI_ENTRY`]，由 gpui-shell 在 GPUI
//!   主线程渲染成原生窗口。
//!
//! Layout: `<data_dir>/bolt/extensions/<id>/extension.json` plus the entry.
//!
//! gpui-shell 自己认的 `gpui-shell.json` **不再使用**：它是一份严格清单
//! （未知字段直接报错，装不下 `kind`/`window` 这类宿主字段），而它对没有清单的
//! 目录会把入口写死为 `main.js`、能力完全交给宿主。于是我们只保留
//! `extension.json`，在 `try_load` 之前由宿主安装能力，代价是 UI 扩展入口必须叫
//! `main.js`（在 [`load_ui_extension`] 里强制）。
//!
//! 目录里若只剩旧布局的 `gpui-shell.json`，或两份清单同时存在，都按错误报告，
//! 不静默处理。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::model::script::Language;

pub const MANIFEST_FILE: &str = "extension.json";
pub const DEFAULT_ENTRY: &str = "index.js";
/// UI 扩展的入口文件名。gpui-shell 对没有清单的目录固定加载它，故不可自定义。
pub const UI_ENTRY: &str = "main.js";
/// gpui-shell 自己的清单名。仅用于识别旧布局，从不被我们读取。
pub const SHELL_MANIFEST_FILE: &str = "gpui-shell.json";
/// 首次启动时生成的示例 UI 扩展目录名。
pub const DEMO_UI_ID: &str = "ui-demo";

/// 每个扩展目录一份的清单。宽松解析（忽略未知字段），只有 `id` 必填。
#[derive(Clone, Debug, Deserialize)]
pub struct ExtensionManifest {
    /// Must match the directory name, so command ids stay stable.
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: ExtensionKind,
    /// 脚本式扩展默认 [`DEFAULT_ENTRY`]；UI 扩展必须是 [`UI_ENTRY`]。
    #[serde(default = "default_script_entry")]
    pub entry: String,
    #[serde(default)]
    pub commands: Vec<ExtensionCommandDef>,
    /// 仅 UI 扩展使用：向宿主要求的授权。
    #[serde(default)]
    pub capabilities: UiCapabilities,
    /// 仅 UI 扩展使用：宿主侧窗口外观。
    #[serde(default)]
    pub window: UiWindowConfig,
}

fn default_script_entry() -> String {
    DEFAULT_ENTRY.to_string()
}

impl ExtensionManifest {
    /// Display name for the launcher group heading.
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// Which runtime renders the extension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionKind {
    #[default]
    Script,
    Ui,
}

/// UI 扩展能要求的授权开关。
///
/// 读根、写根、storage、exit 由宿主固定授予：读 `[扩展目录, 扩展数据目录]`、
/// 写 `[扩展数据目录]`、`storage`、`exit`；网络与子进程一律不授予。
/// 剪贴板是真的需要把关的一项，故单独开成开关。
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UiCapabilities {
    /// 剪贴板读写（读与写同开同关）。
    pub clipboard: bool,
}

/// UI 扩展窗口的宿主侧外观配置。
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct UiWindowConfig {
    /// true → 以 `WindowKind::Floating` 开窗（浮于普通窗口之上，即「置顶」）。
    pub always_on_top: bool,
    pub width: f32,
    pub height: f32,
}

impl Default for UiWindowConfig {
    fn default() -> Self {
        Self {
            always_on_top: false,
            width: 880.,
            height: 720.,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExtensionCommandDef {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
}

/// A successfully discovered **脚本式** extension.
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

/// A discovered **UI extension**. [`Self::entry`] is always `main.js`.
#[derive(Clone, Debug)]
pub struct UiExtension {
    pub manifest: ExtensionManifest,
    /// Absolute path of the extension directory (the gpui-shell application root).
    pub root: PathBuf,
}

impl UiExtension {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn name(&self) -> &str {
        self.manifest.display_name()
    }

    /// Entry module, relative to [`Self::root`]. Always [`UI_ENTRY`].
    pub fn entry(&self) -> &str {
        &self.manifest.entry
    }
}

/// 一次磁盘扫描的产物。
struct Scanned {
    /// 已解析出清单的目录及其清单（`kind` 决定归属）。
    parsed: Vec<(PathBuf, ExtensionManifest)>,
    /// 与 `kind` 无关的目录级错误：旧布局、两份清单同存、清单解析失败。
    /// 只由 [`list_extensions`] 报出，避免两个发现函数重复报同一条。
    errors: Vec<ExtensionError>,
}

/// 扫描扩展目录的一级子目录，返回已解析清单与目录级错误。
///
/// 规则：有 [`MANIFEST_FILE`] 即收录（`kind` 决定归属）；
/// 若同目录还有 [`SHELL_MANIFEST_FILE`] → 报歧义；只有后者 → 报旧布局待迁移；
/// 两者都没有 → 跳过。
fn scan_extensions(dir: &Path) -> Scanned {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Scanned { parsed, errors };
    };
    for entry in entries.flatten() {
        let sub = entry.path();
        if !sub.is_dir() {
            continue;
        }
        let has_manifest = sub.join(MANIFEST_FILE).is_file();
        let has_shell = sub.join(SHELL_MANIFEST_FILE).is_file();
        if !has_manifest {
            if has_shell {
                errors.push(ExtensionError {
                    dir: sub,
                    message: format!(
                        "旧布局：只发现 {SHELL_MANIFEST_FILE}；请把它的字段合并进 {MANIFEST_FILE}（UI 扩展入口固定 {UI_ENTRY}）"
                    ),
                });
            }
            continue;
        }
        if has_shell {
            errors.push(ExtensionError {
                dir: sub,
                message: format!(
                    "同时包含 {MANIFEST_FILE} 与 {SHELL_MANIFEST_FILE}：请删除 {SHELL_MANIFEST_FILE}，只保留 {MANIFEST_FILE}"
                ),
            });
            continue;
        }
        match read_manifest(&sub) {
            Ok(manifest) => parsed.push((sub, manifest)),
            Err(message) => errors.push(ExtensionError { dir: sub, message }),
        }
    }
    Scanned { parsed, errors }
}

fn read_manifest(dir: &Path) -> Result<ExtensionManifest, String> {
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
    Ok(manifest)
}

/// Scan `dir` one level deep for 脚本式 extensions (`kind` omitted or `"script"`).
/// Subdirectories without a manifest are ignored; broken ones are reported but
/// do not affect the rest.
///
/// 目录级错误（旧布局、歧义、解析失败）由本函数报出；UI 扩展自身的加载错误由
/// [`list_ui_extensions`] 报出。
pub fn list_extensions(dir: &Path) -> (Vec<Extension>, Vec<ExtensionError>) {
    let Scanned { parsed, mut errors } = scan_extensions(dir);
    let mut extensions = Vec::new();
    for (sub, manifest) in parsed {
        if manifest.kind != ExtensionKind::Script {
            continue;
        }
        match load_script_extension(&sub, manifest) {
            Ok(extension) => extensions.push(extension),
            Err(message) => errors.push(ExtensionError { dir: sub, message }),
        }
    }
    extensions.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    (extensions, errors)
}

fn load_script_extension(dir: &Path, manifest: ExtensionManifest) -> Result<Extension, String> {
    let entry = dir.join(&manifest.entry);
    if !entry.is_file() {
        return Err(format!("entry file missing: {}", entry.display()));
    }
    if Language::from_path(&entry).is_none() {
        return Err(format!("entry must be .js or .ts: {}", entry.display()));
    }
    Ok(Extension { manifest, entry })
}

/// Scan `dir` one level deep for UI extensions (`"kind": "ui"`). Only UI-specific
/// load errors are returned; directory-level errors belong to [`list_extensions`].
pub fn list_ui_extensions(dir: &Path) -> (Vec<UiExtension>, Vec<ExtensionError>) {
    let Scanned { parsed, .. } = scan_extensions(dir);
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    for (sub, manifest) in parsed {
        if manifest.kind != ExtensionKind::Ui {
            continue;
        }
        match load_ui_extension(&sub, manifest) {
            Ok(extension) => extensions.push(extension),
            Err(message) => errors.push(ExtensionError { dir: sub, message }),
        }
    }
    extensions.sort_by(|a, b| a.id().cmp(b.id()));
    (extensions, errors)
}

fn load_ui_extension(dir: &Path, manifest: ExtensionManifest) -> Result<UiExtension, String> {
    if manifest.entry != UI_ENTRY {
        return Err(format!(
            "UI 扩展的 entry 必须是 {UI_ENTRY}（gpui-shell 对没有 {SHELL_MANIFEST_FILE} 的目录固定加载 {UI_ENTRY}），当前为 `{}`",
            manifest.entry
        ));
    }
    let entry = dir.join(UI_ENTRY);
    if !entry.is_file() {
        return Err(format!("entry file missing: {}", entry.display()));
    }
    if !(manifest.window.width > 0. && manifest.window.height > 0.) {
        return Err(format!(
            "window size must be positive: {}×{}",
            manifest.window.width, manifest.window.height
        ));
    }
    Ok(UiExtension {
        manifest,
        root: dir.to_path_buf(),
    })
}

/// Per-extension private data directory:
/// `~/Library/Application Support/bolt/extensions-data/<id>/`.
///
/// Kept next to `scripts/` and `extensions/` so a user editing an extension can
/// find its storage without knowing about gpui-shell's own layout.
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

const DEMO_UI_MANIFEST: &str = r##"{
  "id": "ui-demo",
  "name": "演示 UI 扩展",
  "kind": "ui",
  "entry": "main.js",
  "window": { "always_on_top": false }
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
    std::fs::write(root.join(MANIFEST_FILE), DEMO_UI_MANIFEST)
        .map_err(|e| format!("write {MANIFEST_FILE}: {e}"))?;
    std::fs::write(root.join(UI_ENTRY), DEMO_UI_MAIN)
        .map_err(|e| format!("write {UI_ENTRY}: {e}"))?;
    Ok(())
}

/// `~/Library/Application Support/bolt/extensions/`.
pub fn extensions_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("extensions");
    path
}

pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create extensions dir: {e}"))
}

/// `base/extensions/` 若存在则返回它。
fn extensions_under(base: &Path) -> Option<PathBuf> {
    let candidate = base.join("extensions");
    candidate.is_dir().then_some(candidate)
}

/// 仓库内随源码维护的扩展目录（开发副本）。
///
/// 先看当前工作目录（`cargo run` 从仓库根启动），再回退编译期包根
/// （直接运行 `target/debug/bolt` 时 CWD 未必在仓库里）。
pub fn bundled_extensions_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    cwd.as_deref()
        .and_then(extensions_under)
        .or_else(|| extensions_under(Path::new(env!("CARGO_MANIFEST_DIR"))))
}

/// [`active_extensions_dir`] 是否直接落在仓库目录里（即 debug 开发形态）。
pub fn uses_bundled_extensions() -> bool {
    cfg!(debug_assertions) && bundled_extensions_dir().is_some()
}

/// 实际生效的扩展目录。
///
/// debug 构建下只要找得到仓库 `extensions/` 就直接用它：改仓库里的扩展文件
/// 重启即生效，省掉「先同步一份到数据目录」这一步（UI 扩展在 debug 下还会热
/// 重载）。release 构建、或找不到仓库目录时用数据目录。
pub fn active_extensions_dir() -> PathBuf {
    let bundled = if cfg!(debug_assertions) {
        bundled_extensions_dir()
    } else {
        None
    };
    bundled.unwrap_or_else(extensions_dir)
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
        if !src.is_dir() {
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
        let (exts, errors) = list_extensions(Path::new("/nonexistent/bolt-test"));
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
        assert_eq!(ext.manifest.kind, ExtensionKind::Script);
        assert_eq!(ext.manifest.entry, DEFAULT_ENTRY);
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

    #[test]
    fn manifest_kind_defaults_to_script() {
        let tmp = TempDir::new("kind-default");
        tmp.write("demo/extension.json", r#"{ "id": "demo" }"#);
        tmp.write("demo/index.js", "");

        let (exts, errors) = list_extensions(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].manifest.kind, ExtensionKind::Script);
        // window/capabilities 未给出时取默认值。
        assert!(!exts[0].manifest.window.always_on_top);
        assert_eq!(exts[0].manifest.window.width, 880.);
        assert_eq!(exts[0].manifest.window.height, 720.);
        assert!(!exts[0].manifest.capabilities.clipboard);
    }

    // ── UI 扩展 ────────────────────────────────────────────────────────────

    const UI_MANIFEST: &str =
        r#"{ "id": "ui", "name": "UI 示例", "kind": "ui", "entry": "main.js" }"#;

    #[test]
    fn loads_ui_extension_and_keeps_it_out_of_script_extensions() {
        let tmp = TempDir::new("ui-load");
        tmp.write("ui/extension.json", UI_MANIFEST);
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
    fn ui_manifest_requires_main_js_entry() {
        let tmp = TempDir::new("ui-entry");
        tmp.write(
            "ui/extension.json",
            r#"{ "id": "ui", "name": "X", "kind": "ui", "entry": "ui.js" }"#,
        );
        tmp.write("ui/ui.js", "");

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert!(ui.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("main.js"));
    }

    #[test]
    fn ui_manifest_requires_main_js_file() {
        let tmp = TempDir::new("ui-entry-missing");
        tmp.write("ui/extension.json", UI_MANIFEST);

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert!(ui.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("entry file missing"));
    }

    #[test]
    fn broken_ui_manifest_is_reported_without_blocking_others() {
        let tmp = TempDir::new("ui-broken");
        // kind 是 ui，但 entry 没写 main.js。
        tmp.write("bad/extension.json", r#"{ "id": "bad", "kind": "ui" }"#);
        tmp.write("ui/extension.json", UI_MANIFEST);
        tmp.write("ui/main.js", "");

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert_eq!(ui.len(), 1);
        assert_eq!(ui[0].id(), "ui");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].dir.ends_with("bad"));
        assert!(!errors[0].message.is_empty());
    }

    #[test]
    fn window_config_parses_and_rejects_bad_json() {
        let tmp = TempDir::new("window-cfg");
        tmp.write(
            "ui/extension.json",
            r#"{
                "id": "ui", "name": "UI", "kind": "ui", "entry": "main.js",
                "capabilities": { "clipboard": true },
                "window": { "always_on_top": true, "width": 1024 }
            }"#,
        );
        tmp.write("ui/main.js", "");

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(ui.len(), 1);
        assert!(ui[0].manifest.window.always_on_top);
        assert_eq!(ui[0].manifest.window.width, 1024.);
        // 未给出的 height 取默认值。
        assert_eq!(ui[0].manifest.window.height, 720.);
        assert!(ui[0].manifest.capabilities.clipboard);

        // 坏 JSON → 目录级错误（由 list_extensions 报出）。
        let bad = TempDir::new("window-cfg-bad");
        bad.write("ui/extension.json", "{ not json");
        let (scripts, errors) = list_extensions(&bad.0);
        assert!(scripts.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("parse"));
    }

    #[test]
    fn zero_window_size_is_an_error() {
        let tmp = TempDir::new("window-zero");
        tmp.write(
            "ui/extension.json",
            r#"{ "id": "ui", "name": "UI", "kind": "ui", "entry": "main.js", "window": { "width": 0 } }"#,
        );
        tmp.write("ui/main.js", "");

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert!(ui.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("window size must be positive"));
    }

    #[test]
    fn legacy_shell_manifest_is_reported() {
        let tmp = TempDir::new("legacy");
        tmp.write(
            "old/gpui-shell.json",
            r#"{ "id": "old", "name": "Old", "entry": "main.js" }"#,
        );
        tmp.write("old/main.js", "");

        let (scripts, errors) = list_extensions(&tmp.0);
        assert!(scripts.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("gpui-shell.json"));
        assert!(errors[0].message.contains("旧布局"));
    }

    #[test]
    fn both_manifests_in_one_directory_is_an_error() {
        let tmp = TempDir::new("ui-both");
        tmp.write("mixed/extension.json", r#"{ "id": "mixed", "kind": "ui" }"#);
        tmp.write("mixed/main.js", "");
        tmp.write("mixed/gpui-shell.json", UI_MANIFEST);

        // 目录级错误由 list_extensions 报出。
        let (scripts, errors) = list_extensions(&tmp.0);
        assert!(scripts.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("gpui-shell.json"));

        // UI 侧同样不收录，也不重复报这条目录级错误。
        let (ui, ui_errors) = list_ui_extensions(&tmp.0);
        assert!(ui.is_empty());
        assert!(ui_errors.is_empty());
    }

    #[test]
    fn demo_ui_extension_is_written_once() {
        let tmp = TempDir::new("ui-demo");
        ensure_demo_ui_extension(&tmp.0).expect("demo should be created");
        let manifest_path = tmp.0.join(DEMO_UI_ID).join(MANIFEST_FILE);
        assert!(manifest_path.is_file());

        let (ui, errors) = list_ui_extensions(&tmp.0);
        assert!(errors.is_empty());
        assert_eq!(ui.len(), 1);
        assert_eq!(ui[0].id(), DEMO_UI_ID);
        assert_eq!(ui[0].entry(), UI_ENTRY);
        assert!(!ui[0].manifest.window.always_on_top);

        // 已存在时不再覆盖作者改动。
        let custom = r#"{ "id": "ui-demo", "name": "自定义", "kind": "ui", "entry": "main.js" }"#;
        std::fs::write(&manifest_path, custom).expect("rewrite manifest");
        ensure_demo_ui_extension(&tmp.0).expect("second call should be a no-op");
        assert_eq!(
            std::fs::read_to_string(&manifest_path).expect("read manifest"),
            custom
        );
    }

    #[test]
    fn extension_data_dir_lives_under_the_app_data_dir() {
        let dir = extension_data_dir("ui-demo");
        assert!(dir.ends_with("bolt/extensions-data/ui-demo"));
    }

    // ── 仓库扩展同步 ────────────────────────────────────────────────────────

    #[test]
    fn bundled_extensions_copy_only_missing() {
        let source = TempDir::new("bundled-src");
        source.write("a/extension.json", r#"{ "id": "a" }"#);
        source.write("a/index.js", "console.log('a');\n");
        source.write("b/extension.json", r#"{ "id": "b" }"#);
        source.write("b/index.js", "console.log('b');\n");

        let target = TempDir::new("bundled-dst");
        let copied = sync_extensions(&source.0, &target.0).expect("first sync");
        assert_eq!(copied, 2);
        assert!(target.0.join("a/index.js").is_file());
        assert!(target.0.join("b/index.js").is_file());

        // 作者改了数据目录里的内容：再次同步不覆盖，也不再复制。
        std::fs::write(target.0.join("a/index.js"), "console.log('local');\n").unwrap();
        let copied = sync_extensions(&source.0, &target.0).expect("second sync");
        assert_eq!(copied, 0);
        assert_eq!(
            std::fs::read_to_string(target.0.join("a/index.js")).unwrap(),
            "console.log('local');\n"
        );
    }

    #[test]
    fn active_extensions_dir_uses_bundled_dir_in_debug() {
        // 测试构建是 debug：仓库 extensions/ 存在时应直接拿它当扩展目录。
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

    #[test]
    fn bundled_extensions_dir_prefers_current_dir() {
        let tmp = TempDir::new("bundled-cwd");
        tmp.write("extensions/demo/extension.json", r#"{ "id": "demo" }"#);
        // 底层查找：给定 base 优先看它的 extensions/。
        assert_eq!(extensions_under(&tmp.0), Some(tmp.0.join("extensions")));
        assert_eq!(extensions_under(Path::new("/nonexistent/bolt-test")), None);
    }
}
