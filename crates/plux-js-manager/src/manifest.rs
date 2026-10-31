//! `extension.json` — 插件清单元数据、校验与目录名约定。
//!
//! 目录名必须是 plux 的 bundle 约定 `{id}-v{version}.{format}`（本 crate 只认
//! `format = "js"`），因为 plux 的 [`Bundle`] 完全从目录名解析出来——`Loader`
//! 拿到的只有一个路径，`Manager` 没有覆盖 bundle 的机会。所以目录名是
//! **唯一事实来源**：清单里的 `id`/`version` 可以省略（省略即取目录名），
//! 写了就必须一致，不允许两边漂移。
//!
//! 命令分两类：带 `ui` 块的命令由 gpui-shell 打开原生窗口（入口固定
//! [`UI_ENTRY`]），不带的在插件常驻运行时里执行同名 JS 函数。

use std::path::{Path, PathBuf};

use plux_rs::{Bundle, Depend};
use semver::{Version, VersionReq};
use serde::Deserialize;

/// 每个插件目录一份的清单文件名。
pub const MANIFEST_FILE: &str = "extension.json";
/// 清单的 JSON Schema 文件名。宿主把它放在**扩展根目录**（与插件目录同级），
/// 于是每个插件里的清单都能用同一个相对路径 `../extension.schema.json` 引用。
pub const SCHEMA_FILE: &str = "extension.schema.json";
/// Schema 正文。唯一来源是仓库根的 `schemas/extension.schema.json`，编译期嵌入：
/// 宿主写盘与测试都用这一份，避免运行时找不到文件。
pub const EXTENSION_SCHEMA: &str = include_str!("../../../schemas/extension.schema.json");
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
    /// 可省略：目录名里已经写着。写了必须与目录名一致。解析后**始终**是目录名
    /// 里的 id（[`read`] 会回填），下游不必再处理"没写"的情况。
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 同 `id`：省略时取目录名里的版本，写了必须一致。
    #[serde(default)]
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
    /// 宿主标题栏右侧的自定义按钮（声明顺序即渲染顺序，图钉固定在最右）。
    pub actions: Vec<UiAction>,
}

impl Default for UiSpec {
    fn default() -> Self {
        Self {
            title: String::new(),
            width: 880.,
            height: 720.,
            always_on_top: false,
            capabilities: UiCapabilities::default(),
            actions: Vec::new(),
        }
    }
}

impl UiSpec {
    /// 窗口标题：清单写了 `ui.title` 就用它，没写（或只有空白）回退到命令标题。
    pub fn display_title<'a>(&'a self, command_title: &'a str) -> &'a str {
        if self.title.trim().is_empty() {
            command_title
        } else {
            &self.title
        }
    }
}

/// `ui.actions[*].builtin` 的合法值。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiBuiltin {
    /// 重新加载插件（与扩展管理里的「重载」同一条路径：会关闭该插件的
    /// 所有 UI 窗口——包括按钮所在的这个——重开命令即用新代码）。
    Reload,
    /// 在访达中打开插件的私有数据目录。
    OpenDataDir,
    /// 在访达中显示（选中）插件目录。
    OpenPluginDir,
    /// 关闭当前窗口。
    CloseWindow,
}

/// 标题栏右侧的自定义按钮：`builtin`（宿主内建动作）与 `command`（点击执行
/// 本清单里的一条**脚本**命令）二选一，校验强制。注意 `command` 型按钮走
/// 插件运行时执行，结果与错误都进 Console——它**无法**回写当前 gpui-shell
/// 窗口的 UI（gpui-shell 尚未提供宿主→插件的事件通道）。
///
/// `icon` / `tooltip` 可选，缺省时按动作取宿主默认值（builtin 用内置图标与
/// 中文提示，command 用 ▶ 图标与命令标题）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UiAction {
    pub builtin: Option<UiBuiltin>,
    pub command: Option<String>,
    /// 图标：Lucide 名（`refresh-cw`）或插件目录内的资产路径（`icons/zap.svg`）。
    pub icon: Option<String>,
    /// 悬停提示文案。
    pub tooltip: Option<String>,
}

/// 一个按钮解析后的语义（[`UiAction::kind`]）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiActionKind {
    Builtin(UiBuiltin),
    /// 引用的脚本命令 id。
    Command(String),
}

impl UiAction {
    /// `builtin` 与 `command` 必须恰好给一个。
    pub fn kind(&self) -> Result<UiActionKind, String> {
        match (&self.builtin, &self.command) {
            (Some(builtin), None) => Ok(UiActionKind::Builtin(*builtin)),
            (None, Some(command)) => Ok(UiActionKind::Command(command.clone())),
            (Some(_), Some(_)) => Err("builtin 与 command 只能写一个".to_string()),
            (None, None) => Err("builtin 与 command 必须写一个".to_string()),
        }
    }

    /// 按钮图标的资产路径；`None` = 用动作的默认图标。
    ///
    /// 带 `/` 或以 `.svg` 结尾的值当成资产路径（相对插件目录，插件根已登记进
    /// 复合资产源），其余当成 Lucide 图标名补成 `icons/<name>.svg`。
    pub fn icon_path(&self) -> Option<String> {
        let icon = self.icon.as_deref().map(str::trim)?;
        if icon.is_empty() {
            return None;
        }
        let icon = icon.trim_start_matches('/');
        if icon.contains('/') || icon.ends_with(".svg") {
            Some(icon.to_string())
        } else {
            Some(format!("icons/{icon}.svg"))
        }
    }

    /// 悬停提示；空白串视同没写。
    pub fn tooltip_text(&self) -> Option<&str> {
        self.tooltip
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
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

    // 目录名是唯一事实来源：id / version 写了就严格比对（漂移是作者最容易踩的
    // 坑），没写（或空串）就用目录名回填。回填放在最前面，后面的 `display_name`
    // 之类才拿得到 id。
    if !manifest.id.is_empty() && manifest.id != bundle.id {
        return Err(format!(
            "清单 id `{}` 与目录名里的 id `{}` 不一致",
            manifest.id, bundle.id
        ));
    }
    manifest.id = bundle.id.clone();

    if !manifest.version.is_empty() {
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
    }
    manifest.version = bundle.version.to_string();

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
        if let Some(ui) = &command.ui {
            if !(ui.width > 0. && ui.height > 0.) {
                return Err(format!(
                    "命令 `{}` 的窗口尺寸必须为正：{}×{}",
                    command.id, ui.width, ui.height
                ));
            }
            for (index, action) in ui.actions.iter().enumerate() {
                let context = format!("命令 `{}` 的 ui.actions[{index}]", command.id);
                // 图标路径交给资产源解析（越界会被拒），这里只挡一眼就能看出的错误。
                if action
                    .icon_path()
                    .is_some_and(|icon| icon.split('/').any(|segment| segment == ".."))
                {
                    return Err(format!("{context}：icon 不能包含 `..`"));
                }
                match action
                    .kind()
                    .map_err(|error| format!("{context}：{error}"))?
                {
                    UiActionKind::Builtin(_) => {}
                    UiActionKind::Command(target_id) => {
                        let Some(target) = manifest
                            .commands
                            .iter()
                            .find(|candidate| candidate.id == target_id)
                        else {
                            return Err(format!("{context}：command `{target_id}` 不存在于本清单"));
                        };
                        if target.is_ui() {
                            return Err(format!(
                                "{context}：command `{target_id}` 是 UI 命令，actions 只能触发脚本命令"
                            ));
                        }
                    }
                }
            }
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
        assert_eq!(ui.display_title(&command.title), "面板");
        assert_eq!((ui.width, ui.height), (880., 720.));
        assert!(!ui.always_on_top);
        assert!(ui.capabilities.clipboard);
        assert!(ui.actions.is_empty());
        assert!(package.main.is_some());
    }

    #[test]
    fn ui_title_overrides_the_command_title() {
        let ui = UiSpec {
            title: "UI窗口".to_string(),
            ..UiSpec::default()
        };
        assert_eq!(ui.display_title("命令标题"), "UI窗口");

        // 只写空白等于没写：不能因为一个空格就让标题栏空着。
        let ui = UiSpec {
            title: "   ".to_string(),
            ..UiSpec::default()
        };
        assert_eq!(ui.display_title("命令标题"), "命令标题");
    }

    #[test]
    fn ui_actions_parse_builtin_and_command() {
        let tmp = TempDir::new("manifest-ui-actions");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [
                {
                    "id": "panel", "title": "面板",
                    "ui": { "actions": [
                        { "builtin": "reload" },
                        { "builtin": "open_data_dir" },
                        { "builtin": "open_plugin_dir" },
                        { "builtin": "close_window" },
                        { "command": "export" }
                    ] }
                },
                { "id": "export", "title": "导出" }
            ]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);

        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        let ui = package.manifest.commands[0].ui.as_ref().expect("ui block");
        let kinds: Vec<UiActionKind> = ui
            .actions
            .iter()
            .map(|action| action.kind().expect("valid action"))
            .collect();
        assert_eq!(
            kinds,
            vec![
                UiActionKind::Builtin(UiBuiltin::Reload),
                UiActionKind::Builtin(UiBuiltin::OpenDataDir),
                UiActionKind::Builtin(UiBuiltin::OpenPluginDir),
                UiActionKind::Builtin(UiBuiltin::CloseWindow),
                UiActionKind::Command("export".to_string()),
            ]
        );
    }

    #[test]
    fn ui_action_must_choose_exactly_one() {
        let tmp = TempDir::new("manifest-ui-action-ambiguous");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": { "actions": [
                { "builtin": "reload", "command": "run" }
            ] } }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);
        let error = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("both fields");
        assert!(error.contains("只能写一个"), "{error}");
    }

    #[test]
    fn ui_action_command_must_reference_a_script_command() {
        let tmp = TempDir::new("manifest-ui-action-command");
        // 指向不存在的命令。
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": { "actions": [
                { "command": "missing" }
            ] } }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);
        let error = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("missing command");
        assert!(error.contains("不存在于本清单"), "{error}");

        // 指向另一条 UI 命令也不行：actions 只能触发脚本命令。
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [
                { "id": "panel", "title": "面板", "ui": { "actions": [ { "command": "other-ui" } ] } },
                { "id": "other-ui", "title": "另一个窗口", "ui": {} }
            ]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);
        let error = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("ui command target");
        assert!(error.contains("只能触发脚本命令"), "{error}");
    }

    #[test]
    fn ui_action_icon_and_tooltip_override_defaults() {
        let tmp = TempDir::new("manifest-ui-action-icon");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": { "actions": [
                { "builtin": "reload", "icon": "zap", "tooltip": "重载并重开" },
                { "builtin": "open_data_dir", "icon": "icons/folder.svg" },
                { "builtin": "open_plugin_dir", "icon": "  ", "tooltip": "   " },
                { "builtin": "close_window" }
            ] } }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);
        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("should load");
        let actions = &package.manifest.commands[0]
            .ui
            .as_ref()
            .expect("ui")
            .actions;

        // Lucide 名补成资产路径。
        assert_eq!(actions[0].icon_path().as_deref(), Some("icons/zap.svg"));
        assert_eq!(actions[0].tooltip_text(), Some("重载并重开"));
        // 已是资产路径的原样保留。
        assert_eq!(actions[1].icon_path().as_deref(), Some("icons/folder.svg"));
        assert_eq!(actions[1].tooltip_text(), None);
        // 空白视同没写：走宿主默认。
        assert_eq!(actions[2].icon_path(), None);
        assert_eq!(actions[2].tooltip_text(), None);
        assert_eq!(actions[3].icon_path(), None);
    }

    #[test]
    fn ui_action_icon_rejects_traversal() {
        let tmp = TempDir::new("manifest-ui-action-traversal");
        let manifest = r#"{
            "id": "demo", "version": "1.0.0",
            "commands": [{ "id": "panel", "title": "面板", "ui": { "actions": [
                { "builtin": "reload", "icon": "../../etc/passwd.svg" }
            ] } }]
        }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js", "main.js"]);
        let error = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("traversal icon");
        assert!(error.contains("不能包含 `..`"), "{error}");
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
    fn id_and_version_may_be_omitted_and_come_from_the_dir_name() {
        // 目录名已经是唯一事实来源：清单里重复一遍 id/version 没有信息量。
        let tmp = TempDir::new("manifest-derive");
        let manifest = r#"{ "name": "演示", "commands": [{ "id": "go", "title": "Go" }] }"#;
        write_plugin(&tmp, "demo-v1.0.0.js", manifest, &["index.js"]);

        let package = read(&tmp.0.join("demo-v1.0.0.js")).expect("dir name fills the gap");
        assert_eq!(package.manifest.id, "demo");
        assert_eq!(package.manifest.version, "1.0.0");

        // 空串等同没写：空 id 本来也没有意义。
        write_plugin(
            &tmp,
            "other-v2.1.0.js",
            r#"{ "id": "", "version": "" }"#,
            &["index.js"],
        );
        let package = read(&tmp.0.join("other-v2.1.0.js")).expect("empty means absent");
        assert_eq!(package.manifest.id, "other");
        assert_eq!(package.manifest.version, "2.1.0");

        // 补出来的命令标题取自展示名 → 退回 id，所以回填必须在补命令之前发生。
        write_plugin(&tmp, "bare-v1.0.0.js", "{ }", &["index.js"]);
        let package =
            read(&tmp.0.join("bare-v1.0.0.js")).expect("empty object is a valid manifest");
        assert_eq!(package.manifest.commands.len(), 1);
        assert_eq!(package.manifest.commands[0].title, "bare");
    }

    #[test]
    fn broken_json_is_reported() {
        let tmp = TempDir::new("manifest-badjson");
        write_plugin(&tmp, "demo-v1.0.0.js", "{ not json", &["index.js"]);
        let err = read(&tmp.0.join("demo-v1.0.0.js")).expect_err("bad json must fail");
        assert!(err.contains("解析"), "unexpected message: {err}");
    }

    fn schema() -> serde_json::Value {
        serde_json::from_str(EXTENSION_SCHEMA).expect("schema must be valid JSON")
    }

    #[test]
    fn schema_enum_matches_the_permission_list() {
        let schema = schema();
        let values: Vec<&str> = schema["properties"]["permissions"]["items"]["enum"]
            .as_array()
            .expect("permissions enum")
            .iter()
            .map(|value| value.as_str().expect("string"))
            .collect();
        // 清单里的权限名与 schema 的补全列表必须同一份，否则作者会照着 schema
        // 写一个 `readFile` 之类根本不存在的名字。
        assert_eq!(values, PERMISSIONS);
    }

    #[test]
    fn schema_enum_matches_the_ui_builtins() {
        let schema = schema();
        let values: Vec<&str> = schema["$defs"]["action"]["properties"]["builtin"]["oneOf"]
            .as_array()
            .expect("builtin oneOf")
            .iter()
            .map(|option| option["const"].as_str().expect("const string"))
            .collect();
        assert_eq!(
            values,
            ["reload", "open_data_dir", "open_plugin_dir", "close_window"]
        );

        // snake_case 是清单里的写法，别在 schema 里抄成 Rust 的变体名。
        for value in values {
            let parsed: Result<UiBuiltin, _> =
                serde_json::from_value(serde_json::Value::String(value.to_string()));
            assert!(parsed.is_ok(), "schema 里的 `{value}` 解析不出 UiBuiltin");
        }
    }

    #[test]
    fn schema_declares_every_field_the_parser_reads() {
        let schema = schema();
        let mut manifest_fields: Vec<&str> = schema["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .map(String::as_str)
            .collect();
        manifest_fields.sort_unstable();
        // PluginManifest 的字段 + `$schema` 自己（宿主忽略，只给编辑器）。
        let mut expected = [
            "$schema",
            "commands",
            "depends",
            "description",
            "entry",
            "id",
            "name",
            "optional_depends",
            "permissions",
            "version",
        ];
        expected.sort_unstable();
        assert_eq!(manifest_fields, expected);

        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        let command = &schema["$defs"]["command"];
        assert_eq!(command["required"], serde_json::json!(["id", "title"]));
        assert_eq!(
            command["properties"]["ui"]["$ref"],
            serde_json::json!("#/$defs/ui")
        );
    }

    #[test]
    fn schema_example_manifest_validates() {
        // README 里的示例（含 ui.actions 的两种按钮与外观覆盖）必须符合 schema：
        // 作者照着文档抄的时候，编辑器不该立刻标红。
        let value: serde_json::Value = serde_json::from_str(
            r#"{
                "$schema": "../extension.schema.json",
                "id": "demo", "name": "演示插件", "description": "同时演示脚本命令与 UI 命令",
                "version": "1.0.0", "entry": "index.js",
                "depends": ["other@^1.0"], "optional_depends": ["extra"],
                "permissions": ["fs", "net"],
                "commands": [
                    { "id": "hello", "title": "打招呼", "description": "打印一行问候" },
                    {
                        "id": "panel", "title": "打开面板",
                        "ui": {
                            "title": "演示面板", "width": 480, "height": 320,
                            "always_on_top": true,
                            "capabilities": { "clipboard": true },
                            "actions": [
                                { "builtin": "reload", "icon": "zap", "tooltip": "重新加载" },
                                { "command": "hello", "icon": "icons/star.svg" }
                            ]
                        }
                    }
                ]
            }"#,
        )
        .expect("json");

        let problems = crate::schema::problems(&schema(), &value);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
