//! `extension.json` — 插件清单元数据、校验与目录名约定。
//!
//! 目录名必须是 plux 的 bundle 约定 `{id}-v{version}.{format}`（本 crate 只认
//! `format = "js"`），因为 plux 的 [`Bundle`] 完全从目录名解析出来。清单里的
//! `id`/`version` 必须与目录名一致：作者改名即改版本，清单与目录不允许漂移。
//!
//! 命令分两类：带 `ui` 块的命令由 gpui-shell 打开原生窗口（入口固定
//! [`UI_ENTRY`]），不带的在插件常驻运行时里执行同名 JS 函数。

use std::path::{Path, PathBuf};

use plux_rs::{Bundle, Depend};
use semver::{Version, VersionReq};
use serde::Deserialize;

/// 每个插件目录一份的清单文件名。
pub const MANIFEST_FILE: &str = "extension.json";
/// 脚本入口的默认名。
pub const DEFAULT_ENTRY: &str = "index.js";
/// UI 命令的入口文件名。gpui-shell 对没有 `gpui-shell.json` 的目录固定加载它，
/// 故不可自定义。
pub const UI_ENTRY: &str = "main.js";
/// 本 manager 认的 bundle 格式（目录名后缀）。
pub const PLUGIN_FORMAT: &str = "js";

/// 脚本命令可声明的权限名。缺省（不写 `permissions`）一个都不给：fail-closed。
///
/// - `fs` — readFile / writeFile / listDir（全盘任意路径）
/// - `net` — fetch（任意网络请求）
/// - `exec` — exec（`sh -c` 任意命令）
/// - `env` — env（读进程环境变量）
/// - `typeStr` — typeStr（向焦点窗口注入键盘输入）
///
/// 交互 API（confirm / pickPath / scanQr / requestFocus）、`sleep`、`exit`
/// 与 console / 定时器不需要声明：它们要么由用户当场交互，要么无副作用。
/// 剪贴板目前只在 UI 命令（gpui-shell）里暴露，由 `ui.capabilities.clipboard`
/// 开关，与这里的权限名互不相干。
pub const PERMISSIONS: &[&str] = &["fs", "net", "exec", "env", "typeStr"];

/// 一个插件目录的清单。宽松解析（忽略未知字段）。
#[derive(Clone, Debug, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 必须与目录名里的版本一致。
    pub version: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    /// 形如 `["other@^1.0"]`，映射到 plux 的 [`Depend`]。
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub optional_depends: Vec<String>,
    /// 脚本命令要用的权限名，取值见 [`PERMISSIONS`]。缺省为空：什么都不给。
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
}

fn default_entry() -> String {
    DEFAULT_ENTRY.to_string()
}

impl PluginManifest {
    /// 展示名：`name` 为空时退回 `id`。
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// 清单里声明的一条命令 = Launcher 里的一条 item。
#[derive(Clone, Debug, Deserialize)]
pub struct PluginCommand {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 给了就是 UI 命令：宿主开原生窗口，不跑脚本命令。
    #[serde(default)]
    pub ui: Option<UiSpec>,
}

impl PluginCommand {
    pub fn is_ui(&self) -> bool {
        self.ui.is_some()
    }
}

/// UI 命令的宿主侧外观与授权。
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct UiSpec {
    /// 为空时用命令标题。
    pub title: String,
    pub width: f32,
    pub height: f32,
    /// true → 开窗即上浮到 floating 层级（置顶图钉仍可切换）。
    pub always_on_top: bool,
    pub capabilities: UiCapabilities,
}

impl Default for UiSpec {
    fn default() -> Self {
        Self {
            title: String::new(),
            width: 880.,
            height: 720.,
            always_on_top: false,
            capabilities: UiCapabilities::default(),
        }
    }
}

impl UiSpec {
    /// 窗口标题：清单没写就用命令标题。
    pub fn display_title<'a>(&'a self, command: &'a PluginCommand) -> &'a str {
        if self.title.is_empty() {
            &command.title
        } else {
            &self.title
        }
    }
}

/// UI 扩展能要求的授权开关。
///
/// 读根、写根、storage、exit 由宿主固定授予；网络与子进程一律不授予。剪贴板是
/// 真的需要把关的一项，故单独开成开关。
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UiCapabilities {
    pub clipboard: bool,
}

/// 校验通过的插件目录：清单 + 从目录名解析出的 bundle + 绝对路径。
///
/// 名字避开 `plux_rs::Plugin`（那是已注册的插件实例）。
#[derive(Clone, Debug)]
pub struct PluginPackage {
    pub manifest: PluginManifest,
    pub bundle: Bundle,
    /// 插件目录（绝对路径）。
    pub root: PathBuf,
    /// 脚本入口的绝对路径。纯 UI 插件（没有脚本命令）为 `None`——这种插件
    /// 根本不需要运行时，也就不该被迫在目录里放一个 `index.js`。
    pub entry: Option<PathBuf>,
    /// UI 入口（有 `ui` 命令时才存在）。
    pub main: Option<PathBuf>,
    pub depends: Vec<Depend>,
    pub optional_depends: Vec<Depend>,
}

impl PluginPackage {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn display_name(&self) -> &str {
        self.manifest.display_name()
    }
}

/// 从插件目录名解析 bundle：`{id}-v{version}.{format}`。
pub fn bundle_from_dir(dir: &Path) -> Result<Bundle, String> {
    let name = dir
        .file_name()
        .ok_or_else(|| format!("非法的插件目录名：{}", dir.display()))?;
    let bundle = Bundle::from_filename(name).map_err(|e| {
        format!(
            "插件目录名必须是 `<id>-v<version>.{PLUGIN_FORMAT}>`（如 `demo-v1.0.0.js`）：{}（{e}）",
            dir.display()
        )
    })?;
    if bundle.format != PLUGIN_FORMAT {
        return Err(format!(
            "插件目录名的格式后缀必须是 `.{PLUGIN_FORMAT}`：{}",
            dir.display()
        ));
    }
    Ok(bundle)
}

/// 读取并校验一个插件目录。
pub fn read(dir: &Path) -> Result<PluginPackage, String> {
    let bundle = bundle_from_dir(dir)?;

    let manifest_path = dir.join(MANIFEST_FILE);
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("读取 {MANIFEST_FILE} 失败：{e}"))?;
    let mut manifest: PluginManifest =
        serde_json::from_str(&raw).map_err(|e| format!("解析 {MANIFEST_FILE} 失败：{e}"))?;

    if manifest.id != bundle.id {
        return Err(format!(
            "清单 id `{}` 与目录名里的 id `{}` 不一致",
            manifest.id, bundle.id
        ));
    }
    let declared = Version::parse(&manifest.version).map_err(|e| {
        format!(
            "清单 version `{}` 不是合法语义化版本：{e}",
            manifest.version
        )
    })?;
    if declared != bundle.version {
        return Err(format!(
            "清单 version `{}` 与目录名里的版本 `{}` 不一致",
            manifest.version, bundle.version
        ));
    }

    if manifest.commands.is_empty() {
        manifest.commands.push(PluginCommand {
            id: "run".to_string(),
            title: manifest.display_name().to_string(),
            description: manifest.description.clone(),
            ui: None,
        });
    }

    let mut seen = Vec::new();
    for command in &manifest.commands {
        if command.id.trim().is_empty() {
            return Err("命令 id 不能为空".to_string());
        }
        if seen.contains(&command.id) {
            return Err(format!("命令 id 重复：{}", command.id));
        }
        seen.push(command.id.clone());
        if let Some(ui) = &command.ui
            && !(ui.width > 0. && ui.height > 0.)
        {
            return Err(format!(
                "命令 `{}` 的窗口尺寸必须为正：{}×{}",
                command.id, ui.width, ui.height
            ));
        }
    }

    let has_script = manifest.commands.iter().any(|c| !c.is_ui());
    let has_ui = manifest.commands.iter().any(|c| c.is_ui());

    // 脚本入口只在真的要用时校验：纯 UI 插件没有入口这个概念。
    let entry = if has_script {
        let entry = dir.join(&manifest.entry);
        if !entry.is_file() {
            return Err(format!("入口文件不存在：{}", entry.display()));
        }
        if !matches!(
            entry.extension().and_then(|e| e.to_str()),
            Some("js" | "ts")
        ) {
            return Err(format!("入口必须是 .js 或 .ts：{}", entry.display()));
        }
        Some(entry)
    } else {
        None
    };

    // UI 入口固定 main.js：gpui-shell 对没有 gpui-shell.json 的目录只认它。
    let main = dir.join(UI_ENTRY);
    if has_ui && !main.is_file() {
        return Err(format!(
            "有 `ui` 命令的插件必须提供 {UI_ENTRY}：{}",
            main.display()
        ));
    }
    let main = has_ui.then_some(main);

    let depends = parse_depends(&manifest.depends)?;
    let optional_depends = parse_depends(&manifest.optional_depends)?;

    // 权限名从紧校验：拼错的权限名如果不报错，作者会以为已经拿到能力。
    for permission in &manifest.permissions {
        if !PERMISSIONS.contains(&permission.as_str()) {
            return Err(format!(
                "未知权限 `{permission}`（清单 permissions）：合法值为 {}",
                PERMISSIONS.join(", ")
            ));
        }
    }

    Ok(PluginPackage {
        manifest,
        bundle,
        root: dir.to_path_buf(),
        entry,
        main,
        depends,
        optional_depends,
    })
}

/// `"id@^1.0"` → [`Depend`]；不带 `@` 表示任意版本。
fn parse_depends(specs: &[String]) -> Result<Vec<Depend>, String> {
    specs
        .iter()
        .map(|spec| {
            let (id, req) = match spec.split_once('@') {
                Some((id, req)) => (id.trim(), req.trim()),
                None => (spec.trim(), "*"),
            };
            if id.is_empty() {
                return Err(format!("依赖声明缺少插件 id：`{spec}`"));
            }
            let req = VersionReq::parse(req)
                .map_err(|e| format!("依赖 `{spec}` 的版本要求不合法：{e}"))?;
            Ok(Depend {
                id: id.to_string(),
                version: req,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    const OK_MANIFEST: &str = r#"{
        "id": "demo", "name": "演示插件", "version": "1.0.0",
        "entry": "index.js",
        "commands": [{ "id": "hello", "title": "打招呼" }]
    }"#;

    fn write_plugin(tmp: &TempDir, dir: &str, manifest: &str, files: &[&str]) {
        tmp.write(&format!("{dir}/{MANIFEST_FILE}"), manifest);
        for file in files {
            tmp.write(&format!("{dir}/{file}"), "");
        }
    }

    #[test]
    fn reads_valid_plugin() {
        let tmp = TempDir::new("manifest-ok");
        write_plugin(&tmp, "demo-v1.0.0.js", OK_MANIFEST, &["index.js"]);

        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        assert_eq!(package.bundle.id, "demo");
        assert_eq!(package.bundle.version.to_string(), "1.0.0");
        assert_eq!(package.bundle.format, "js");
        assert_eq!(package.display_name(), "演示插件");
        assert!(package.entry.is_some_and(|e| e.ends_with("index.js")));
        assert!(package.main.is_none());
        assert_eq!(package.manifest.commands.len(), 1);
    }

    #[test]
    fn ui_only_plugin_needs_no_script_entry() {
        let tmp = TempDir::new("manifest-ui-only");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": {} }]
        }"#;
        // 没有 index.js：纯 UI 插件不该被迫放一个。
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["main.js"]);

        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("ui-only plugin should load");
        assert!(package.entry.is_none());
        assert!(package.main.is_some());
    }

    #[test]
    fn dir_name_must_carry_id_and_version() {
        let tmp = TempDir::new("manifest-baddir");
        write_plugin(&tmp, "demo", OK_MANIFEST, &["index.js"]);

        let err = read(&tmp.0.join("demo")).expect_err("bare dir name must fail");
        assert!(err.contains("demo-v1.0.0.js"), "unexpected message: {err}");
    }

    #[test]
    fn format_suffix_must_be_js() {
        let tmp = TempDir::new("manifest-badformat");
        write_plugin(&tmp, "demo-v1.0.0.lua", OK_MANIFEST, &["index.js"]);

        let err = read(&tmp.0.join("demo-v1.0.0.lua")).expect_err("lua must fail");
        assert!(err.contains(".js"), "unexpected message: {err}");
    }

    #[test]
    fn id_and_version_must_match_dir_name() {
        let tmp = TempDir::new("manifest-mismatch");
        write_plugin(&tmp, "demo-v1.0.0.js", OK_MANIFEST, &["index.js"]);
        let renamed = tmp.0.join("other-v1.0.0.js");
        std::fs::rename(tmp.0.join("demo-v1.0.0.js"), &renamed).unwrap();
        let err = read(&renamed).expect_err("id mismatch must fail");
        assert!(err.contains("不一致"), "unexpected message: {err}");

        let tmp = TempDir::new("manifest-version-mismatch");
        write_plugin(&tmp, "demo-v2.0.0.js", OK_MANIFEST, &["index.js"]);
        let err = read(&tmp.0.join("demo-v2.0.0.js")).expect_err("version mismatch must fail");
        assert!(err.contains("不一致"), "unexpected message: {err}");
    }

    #[test]
    fn entry_must_exist_and_be_javascript() {
        let tmp = TempDir::new("manifest-noentry");
        write_plugin(&tmp, "demo-v1.0.0.js", OK_MANIFEST, &[]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("missing entry must fail");
        assert!(err.contains("入口文件不存在"), "unexpected message: {err}");

        let tmp = TempDir::new("manifest-badext");
        let manifest = r#"{ "id": "demo", "version": "1.0.0", "entry": "run.py" }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["run.py"]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("python entry must fail");
        assert!(
            err.contains("必须是 .js 或 .ts"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn empty_commands_synthesize_a_run_command() {
        let tmp = TempDir::new("manifest-nocommands");
        let manifest = r#"{ "id": "demo", "name": "Demo", "version": "1.0.0" }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js"]);

        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        assert_eq!(package.manifest.commands.len(), 1);
        assert_eq!(package.manifest.commands[0].id, "run");
        assert_eq!(package.manifest.commands[0].title, "Demo");
    }

    #[test]
    fn ui_command_requires_main_js_and_positive_size() {
        let tmp = TempDir::new("manifest-ui-nomain");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": {} }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js"]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("missing main.js must fail");
        assert!(err.contains("main.js"), "unexpected message: {err}");

        let tmp = TempDir::new("manifest-ui-zerosize");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": { "width": 0 } }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("zero width must fail");
        assert!(err.contains("窗口尺寸"), "unexpected message: {err}");
    }

    #[test]
    fn ui_command_defaults_and_title_fallback() {
        let tmp = TempDir::new("manifest-ui-defaults");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": { "capabilities": { "clipboard": true } } }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);

        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        let command = &package.manifest.commands[0];
        let ui = command.ui.as_ref().expect("ui block");
        assert_eq!(ui.display_title(command), "面板");
        assert_eq!((ui.width, ui.height), (880., 720.));
        assert!(!ui.always_on_top);
        assert!(ui.capabilities.clipboard);
        assert!(package.main.is_some());
    }

    #[test]
    fn duplicate_command_ids_are_rejected() {
        let tmp = TempDir::new("manifest-dupe");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "a", "title": "A" }, { "id": "a", "title": "B" }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js"]);

        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("duplicate ids must fail");
        assert!(err.contains("重复"), "unexpected message: {err}");
    }

    #[test]
    fn depends_parse_into_version_requirements() {
        let tmp = TempDir::new("manifest-depends");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "depends": ["base@^1.0"],
            "optional_depends": ["extra"]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js"]);

        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        assert_eq!(package.depends.len(), 1);
        assert_eq!(package.depends[0].id, "base");
        assert!(package.depends[0].version.matches(&Version::new(1, 4, 0)));
        assert_eq!(package.optional_depends[0].id, "extra");

        let bad = r#"{ "id": "demo", "version": "1.0.0", "depends": ["base@??"] }"#;
        let tmp = TempDir::new("manifest-baddepends");
        write_plugin(&tmp, "demo-v1.0.0.js", bad, &["index.js"]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("bad req must fail");
        assert!(err.contains("版本要求"), "unexpected message: {err}");
    }

    #[test]
    fn permissions_default_to_empty_and_validate_names() {
        // 缺省全关：没写 permissions 的清单解析为空列表。
        let tmp = TempDir::new("manifest-perm-default");
        write_plugin(&tmp, "demo-v1.0.0.js", OK_MANIFEST, &["index.js"]);
        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        assert!(package.manifest.permissions.is_empty());

        // 声明合法权限原样保留。
        let tmp = TempDir::new("manifest-perm-ok");
        let manifest = r#"{ "id": "demo", "version": "1.0.0", "permissions": ["fs", "net"] }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js"]);
        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        assert_eq!(package.manifest.permissions, ["fs", "net"]);

        // 拼错的权限名必须报错，否则作者会以为已经拿到能力。
        let tmp = TempDir::new("manifest-perm-typo");
        let manifest = r#"{ "id": "demo", "version": "1.0.0", "permissions": ["fss"] }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js"]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("typo must fail");
        assert!(
            err.contains("未知权限") && err.contains("fs, net, exec, env, typeStr"),
            "{err}"
        );
    }

    #[test]
    fn broken_json_is_reported() {
        let tmp = TempDir::new("manifest-badjson");
        write_plugin(&tmp, "demo-v1.0.0.js", "{ not json", &["index.js"]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("bad json must fail");
        assert!(err.contains("解析"), "unexpected message: {err}");
    }
}
