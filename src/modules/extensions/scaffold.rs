//! 「新建扩展」的文件生成：校验 → 模板 → 写盘 → 类型声明。
//!
//! 扩展目录就是 `<id>/`（目录名 = 扩展 id），id 规则与清单校验共用
//! `bolt_extensions::manifest::valid_id` 这一份。
//!
//! 生成物自带类型提示：脚本模板写 `bolt.d.ts`（宿主模块的 `declare module`），
//! UI 模板写 `gpui-kit.d.ts` + `jsconfig.json`（gpui-shell 的编辑器配置）。

use std::path::{Path, PathBuf};

use super::manifest::{MANIFEST_FILE, active_extensions_dir};
use crate::host::HostRegistry;
use crate::model::project_gen::write_if_changed;

/// 生成哪些命令。纯 UI 扩展不需要 `index.js`，所以模板不只是"文件多少"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Template {
    Script,
    Ui,
    Both,
}

impl Template {
    pub(crate) const ALL: [Template; 3] = [Template::Script, Template::Ui, Template::Both];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Template::Script => "脚本命令",
            Template::Ui => "UI 窗口",
            Template::Both => "两者都要",
        }
    }

    pub(crate) fn has_script(self) -> bool {
        matches!(self, Template::Script | Template::Both)
    }

    pub(crate) fn has_ui(self) -> bool {
        matches!(self, Template::Ui | Template::Both)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ExtensionSpec {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) template: Template,
    pub(crate) permissions: Vec<String>,
    pub(crate) clipboard: bool,
}

impl ExtensionSpec {
    fn display_name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            self.id.trim().to_string()
        } else {
            name.to_string()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Id,
    Name,
    Version,
}

#[derive(Clone, Debug)]
pub(crate) struct SpecError {
    pub(crate) field: Field,
    pub(crate) message: String,
}

/// 校验表单输入。字段级错误一次全给，表单可以标出每一行。
///
/// id 规则与清单校验共用 `valid_id`；version 非空时必须是合法语义化版本，
/// 留空则清单侧按缺省 `1.0.0` 处理。
pub(crate) fn validate(spec: &ExtensionSpec) -> Result<(), Vec<SpecError>> {
    let mut errors = Vec::new();
    let id = spec.id.trim();
    let version = spec.version.trim();

    if let Err(message) = bolt_extensions::manifest::valid_id(id) {
        errors.push(SpecError {
            field: Field::Id,
            message,
        });
    }

    let name = spec.name.trim();
    if name.chars().count() > 64 {
        errors.push(SpecError {
            field: Field::Name,
            message: "名称最长 64 个字符（留空则用 id）".to_string(),
        });
    }

    if !version.is_empty() && semver::Version::parse(version).is_err() {
        errors.push(SpecError {
            field: Field::Version,
            message: format!("`{version}` 不是合法语义化版本（示例：1.0.0）"),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// 目标目录（[`active_extensions_dir`] 下）：目录名就是扩展 id。
///
/// 必须跟着**实际生效**的目录走：debug 构建下宿主扫的是仓库 `extensions/`，
/// 若写到用户数据目录，扩展建完永远不会被扫描到（Launcher 里也没有命令）。
pub(crate) fn target_dir(spec: &ExtensionSpec) -> PathBuf {
    active_extensions_dir().join(spec.id.trim())
}

/// 目录已存在时不允许覆盖：请换一个 id，或先卸载旧扩展。
pub(crate) fn conflict(spec: &ExtensionSpec) -> Option<String> {
    conflict_in(&target_dir(spec))
}

/// [`conflict`] 的路径版：UI 与 `scaffold` 用同一句文案，别各写一份。
fn conflict_in(root: &Path) -> Option<String> {
    root.exists().then(|| {
        format!(
            "目录已存在：{}\n请换一个 id，或先卸载旧扩展。",
            root.display()
        )
    })
}

/// 要写进扩展目录的文件（相对路径 + 内容）。纯函数，方便单测比对。
pub(crate) fn files(spec: &ExtensionSpec) -> Vec<(&'static str, String)> {
    let mut files = vec![(MANIFEST_FILE, manifest_json(spec))];
    if spec.template.has_script() {
        files.push(("index.js", script_entry()));
    }
    if spec.template.has_ui() {
        files.push(("main.js", ui_entry(spec)));
    }
    // 这三样都是生成物：`bolt.d.ts` / `gpui-kit.d.ts` 随宿主版本重写，
    // `jsconfig.json` 由 gpui-shell 写一次，作者改过就不再覆盖。
    files.push((
        ".gitignore",
        "bolt.d.ts\ngpui-kit.d.ts\njsconfig.json\n".to_string(),
    ));
    files
}

fn manifest_json(spec: &ExtensionSpec) -> String {
    let name = spec.display_name();
    let mut commands = Vec::new();
    if spec.template.has_script() {
        commands.push(serde_json::json!({ "id": "run", "title": name }));
    }
    if spec.template.has_ui() {
        commands.push(serde_json::json!({
            "id": "panel",
            "title": name,
            "ui": { "title": name, "width": 720, "height": 480 }
        }));
    }

    let mut manifest = serde_json::json!({
        // 编辑器提示：schema 由宿主写在扩展根目录（见 `ensure_schema`）。
        "$schema": "../extension.schema.json",
        "id": spec.id.trim(),
        "name": name,
        "version": spec.version.trim(),
        "permissions": spec.permissions,
        "commands": commands,
    });
    // 纯 UI 扩展没有脚本入口：不写 entry，免得留下一个指向不存在文件的字段。
    if spec.template.has_script() {
        manifest["entry"] = serde_json::json!("index.js");
    }
    if spec.clipboard && spec.template.has_ui() {
        manifest["commands"][if spec.template.has_script() { 1 } else { 0 }]["ui"]["capabilities"] =
            serde_json::json!({ "clipboard": true });
    }

    format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".to_string())
    )
}

fn script_entry() -> String {
    r#"// 脚本命令示例：命令 id 就是这里导出的函数名（清单里声明的是 `run`）。
//
// 返回值会打印到 Console；模块级状态会保留到下次调用。
// 宿主注入的模块见同目录的 `bolt.d.ts`（编辑器里能直接补全）。

/**
 * @typedef {{ extension: { id: string, version: string }, command: string }} CommandContext
 */

/**
 * @param {CommandContext} ctx
 * @returns {string}
 */
export function run(ctx) {
  return `你好，这是 ${ctx.extension.id} v${ctx.extension.version} 的 ${ctx.command} 命令`;
}
"#
    .to_string()
}

fn ui_entry(spec: &ExtensionSpec) -> String {
    let title = spec.display_name();
    format!(
        r#"// UI 命令示例：脚本只描述界面，渲染由宿主在 GPUI 主线程完成。
//
// 可用模块：`gpui-kit`、`gpui-base`、`gpui-fps`；类型提示见同目录的
// `gpui-kit.d.ts`（由 gpui-shell 生成，随运行时升级自动重写）。
// 保存后窗口会热重载（debug 构建）。
import {{ View, div }} from "gpui-kit";
import {{ v_flex }} from "gpui-base";

export default class Panel extends View {{
  init() {{}}

  /**
   * @param {{any}} cx
   */
  render(cx) {{
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .p(24)
      .gap(8)
      .bg(colors.background)
      .child(
        div()
          .text_size(16)
          .font_semibold()
          .text_color(colors.foreground)
          .child({title:?}),
      )
      .child(
        div()
          .text_size(12)
          .text_color(colors.muted_foreground)
          .child("编辑 main.js 保存即可看到改动"),
      );
  }}
}}
"#
    )
}

/// 写入类型声明：脚本模板要 `bolt.d.ts`，UI 模板要 `gpui-kit.d.ts`
/// （gpui-shell 会在目录里没有任何配置时顺带写 `jsconfig.json`）。
pub(crate) fn write_declarations(
    dir: &Path,
    spec: &ExtensionSpec,
    registry: &HostRegistry,
) -> Result<Vec<PathBuf>, String> {
    let mut written = Vec::new();

    if spec.template.has_script() {
        registry
            .write_type_declarations(dir)
            .map_err(|error| format!("写入 bolt.d.ts 失败：{error}"))?;
        written.push(dir.join("bolt.d.ts"));
    }

    if spec.template.has_ui() {
        let files = gpui_shell::write_type_declarations_with_components(
            dir,
            &gpui_shell::FrozenComponentRegistry::default(),
        )
        .map_err(|error| format!("写入 gpui-kit.d.ts 失败：{error}"))?;
        written.extend(files);
    }

    Ok(written)
}

#[derive(Clone, Debug)]
pub(crate) struct ScaffoldOutcome {
    pub(crate) root: PathBuf,
}

/// 真正落盘：校验 → 查冲突 → 建目录 → 写文件 → 写声明。
///
/// 顺序有讲究：`jsconfig.json` 不被 `dir_fingerprint` 忽略，必须和清单、入口
/// 一次性写完，否则第一次 refresh 就会把新扩展判成"内容已变更"。
///
/// 写盘失败会把刚建的目录整个删掉：否则半成品一直占着这个 `<id>/`，
/// 用户修好问题再点「创建」只会得到「目录已存在」，再也无法重试。
pub(crate) fn scaffold(
    dir: &Path,
    spec: &ExtensionSpec,
    registry: &HostRegistry,
) -> Result<ScaffoldOutcome, String> {
    validate(spec).map_err(|errors| {
        errors
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>()
            .join("；")
    })?;

    let root = dir.join(spec.id.trim());
    if let Some(conflict) = conflict_in(&root) {
        return Err(conflict);
    }

    std::fs::create_dir_all(&root)
        .map_err(|error| format!("创建 {} 失败：{error}", root.display()))?;

    if let Err(error) = write_contents(&root, spec, registry) {
        let _ = std::fs::remove_dir_all(&root);
        return Err(error);
    }
    Ok(ScaffoldOutcome { root })
}

fn write_contents(dir: &Path, spec: &ExtensionSpec, registry: &HostRegistry) -> Result<(), String> {
    for (relative, content) in files(spec) {
        write_if_changed(&dir.join(relative), &content)?;
    }
    write_declarations(dir, spec, registry)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::extensions::manifest::{bundled_extensions_dir, uses_bundled_extensions};
    use crate::test_support::TempDir;
    use bolt_extensions::MANIFEST_FILE;

    fn spec(id: &str, template: Template) -> ExtensionSpec {
        ExtensionSpec {
            id: id.to_string(),
            name: "演示".to_string(),
            version: "1.0.0".to_string(),
            template,
            permissions: vec!["env".to_string()],
            clipboard: false,
        }
    }

    fn registry() -> HostRegistry {
        let mut registry = HostRegistry::new();
        crate::host::modules::register_all(&mut registry);
        registry
    }

    fn scaffold_into(tmp: &TempDir, spec: &ExtensionSpec) -> ScaffoldOutcome {
        scaffold(&tmp.0, spec, &registry()).expect("scaffold")
    }

    #[test]
    fn script_template_is_a_valid_extension() {
        let tmp = TempDir::new("scaffold-script");
        let outcome = scaffold_into(&tmp, &spec("hello", Template::Script));

        // 最有价值的一条：生成物本身就是运行时能读的扩展。
        let package = bolt_extensions::manifest::read(&outcome.root).expect("valid manifest");
        assert_eq!(package.manifest.id, "hello");
        assert_eq!(package.manifest.commands.len(), 1);
        assert!(package.entry.is_some());
        assert!(outcome.root.join("bolt.d.ts").is_file());
        assert!(!outcome.root.join("gpui-kit.d.ts").exists());
    }

    #[test]
    fn ui_template_has_main_and_declarations() {
        let tmp = TempDir::new("scaffold-ui");
        let outcome = scaffold_into(&tmp, &spec("panel", Template::Ui));

        let package = bolt_extensions::manifest::read(&outcome.root).expect("valid manifest");
        assert!(package.entry.is_none(), "纯 UI 扩展不该有脚本入口");
        assert!(package.main.is_some());
        assert!(outcome.root.join("main.js").is_file());
        assert!(outcome.root.join("gpui-kit.d.ts").is_file());
        assert!(outcome.root.join("jsconfig.json").is_file());
        // 纯 UI 里没有 `bolt` 模块，写 bolt.d.ts 只会误导编辑器。
        assert!(!outcome.root.join("bolt.d.ts").exists());
    }

    #[test]
    fn both_template_declares_two_commands() {
        let tmp = TempDir::new("scaffold-both");
        let outcome = scaffold_into(&tmp, &spec("combo", Template::Both));

        let package = bolt_extensions::manifest::read(&outcome.root).expect("valid manifest");
        assert_eq!(package.manifest.commands.len(), 2);
        assert!(outcome.root.join("index.js").is_file());
        assert!(outcome.root.join("main.js").is_file());
        assert!(outcome.root.join("bolt.d.ts").is_file());
        assert!(outcome.root.join("gpui-kit.d.ts").is_file());
    }

    #[test]
    fn id_with_uppercase_or_space_is_rejected() {
        for id in ["Hello", "he llo", "-hello", "hello_"] {
            let errors = validate(&spec(id, Template::Script)).expect_err("must reject");
            assert_eq!(errors[0].field, Field::Id, "{id}: {errors:?}");
        }
    }

    #[test]
    fn version_must_be_semver() {
        let mut spec = spec("demo", Template::Script);
        spec.version = "1.0".to_string();
        let errors = validate(&spec).expect_err("must reject");
        assert_eq!(errors[0].field, Field::Version);
    }

    #[test]
    fn existing_directory_is_refused() {
        let tmp = TempDir::new("scaffold-conflict");
        let spec = spec("demo", Template::Script);
        scaffold_into(&tmp, &spec);
        let error = scaffold(&tmp.0, &spec, &registry()).expect_err("must refuse");
        assert!(error.contains("目录已存在"), "{error}");
    }

    #[test]
    fn the_create_target_follows_the_active_extensions_dir() {
        let spec = spec("demo", Template::Script);
        let target = target_dir(&spec);
        // 表单里的「目标目录」预览、冲突检查、落盘三处都从这里取，必须与宿主
        // 扫描的目录是同一份；debug 构建里宿主扫的是仓库 extensions/。
        assert_eq!(target.parent(), Some(active_extensions_dir().as_path()));
        if uses_bundled_extensions() {
            let repo = bundled_extensions_dir().expect("debug 下能定位仓库目录");
            assert!(
                target.starts_with(&repo),
                "debug 构建应写进仓库 extensions/，实际是 {}",
                target.display()
            );
        }
    }

    #[test]
    fn the_form_preview_and_the_refusal_use_the_same_words() {
        let tmp = TempDir::new("scaffold-conflict-text");
        let spec = spec("demo", Template::Script);
        let root = tmp.0.join(spec.id.trim());
        assert_eq!(conflict_in(&root), None, "还没建的时候不该报冲突");

        scaffold_into(&tmp, &spec);
        let preview = conflict_in(&root).expect("表单能提前看到冲突");
        let refusal = scaffold(&tmp.0, &spec, &registry()).expect_err("must refuse");
        // 表单里那行红字就是点「创建」会拿到的同一条文案，别一边一个说法。
        assert_eq!(preview, refusal);
    }

    #[test]
    fn manifest_mentions_declared_files() {
        let spec = spec("demo", Template::Script);
        let manifest = files(&spec)
            .into_iter()
            .find(|(name, _)| *name == MANIFEST_FILE)
            .map(|(_, content)| content)
            .expect("manifest");
        let value: serde_json::Value = serde_json::from_str(&manifest).expect("json");
        assert_eq!(value["entry"], "index.js");
        assert_eq!(value["permissions"][0], "env");
    }
}
