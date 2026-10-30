//! plux 插件宿主：`Loader`（生命周期/元数据）+ `PluginStore`（执行）。
//!
//! **`Loader` 必须先 `Box` 再注册插件，之后永不移动。** plux 的 `Plugin` 里存的是
//! 指向 `loader.managers` Vec 槽位的裸指针（`Ptr`），在栈上建好 Loader 再 move 进
//! 结构体会让它立刻悬空。同理，manager 只在插件注册之前注册——之后不再往
//! `managers` 里 push，Vec 不会 realloc，指针一直有效。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use plux_js_manager::{JsEnv, JsManager, PluginMeta, PluginStore, UiSpec};
use plux_rs::function::FunctionOutput;
use plux_rs::utils::RegisterPluginError;
use plux_rs::{Bundle, Loader};

use super::manifest;

pub(crate) type ExtensionsLoader = Loader<'static, FunctionOutput, PluginMeta>;

/// Launcher 里的一条命令（已脱离 `Loader` 的借用，可 move 进 `on_confirm`）。
#[derive(Clone, Debug)]
pub(crate) struct CommandTarget {
    pub(crate) bundle: Bundle,
    pub(crate) plugin_id: String,
    pub(crate) plugin_name: String,
    /// 插件目录：UI 命令用它做 gpui-shell 的应用根。
    pub(crate) root: PathBuf,
    pub(crate) command_id: String,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    /// 给了就是 UI 命令。
    pub(crate) ui: Option<UiSpec>,
}

impl CommandTarget {
    /// Launcher item 的 id：`ext.<plugin-id>.<command-id>`。
    pub fn item_id(&self) -> String {
        format!("ext.{}.{}", self.plugin_id, self.command_id)
    }

    pub(crate) fn is_ui(&self) -> bool {
        self.ui.is_some()
    }
}

pub(crate) struct ExtensionsHost {
    /// 见模块头部：必须钉在 Box 里。
    loader: Box<ExtensionsLoader>,
    store: Arc<PluginStore>,
    extensions_dir: PathBuf,
    /// 注册 manager 失败的原因。此时宿主仍然可构造，但所有命令都不可用，
    /// 由 `refresh` 把这条错误报给 Console。
    init_error: Option<String>,
}

impl ExtensionsHost {
    pub(crate) fn new(extensions_dir: &Path, env: Arc<dyn JsEnv>) -> Self {
        let manager = JsManager::new(env);
        let store = manager.store().clone();

        let mut loader: Box<ExtensionsLoader> = Box::new(Loader::new());
        let init_error = loader
            .context(|mut ctx| ctx.register_manager(manager))
            .err()
            .map(|e| format!("注册 JS 插件管理器失败：{e}"));

        Self {
            loader,
            store,
            extensions_dir: extensions_dir.to_path_buf(),
            init_error,
        }
    }

    /// 重新扫描磁盘并注册新插件；返回本次发现的错误（不含已注册插件的重复注册）。
    pub(crate) fn refresh(&mut self) -> Vec<String> {
        if let Some(error) = &self.init_error {
            return vec![error.clone()];
        }

        let (dirs, mut errors) = manifest::scan(&self.extensions_dir);
        for dir in dirs {
            println!("dir: {:?}", dir);
            let Some(path) = dir.to_str() else {
                errors.push(format!("插件路径不是合法 UTF-8：{}", dir.display()));
                continue;
            };
            match self.loader.register_plugin(path) {
                // 重复注册（同 id 同版本）是正常的：Launcher 每次打开都会刷新。
                Ok(_) | Err(RegisterPluginError::AlreadyExistsIDAndVersion(..)) => {}
                Err(error) => errors.push(format!(
                    "扩展加载失败 {}：{}",
                    dir.display(),
                    describe(&error)
                )),
            }
        }
        errors
    }

    /// 当前所有插件贡献的命令，按注册顺序（即插件目录名排序）。
    pub(crate) fn commands(&self) -> Vec<CommandTarget> {
        let mut targets = Vec::new();
        for plugin in self.loader.get_plugins() {
            let info = plugin.info();
            let meta = &info.info;
            println!("plugin id: {}", meta.id());
            for command in meta.commands() {
                // UI 标题在清单里可省；省了就用命令标题，免得下游到处判断空串。
                let ui = command.ui.clone().map(|mut ui| {
                    if ui.title.is_empty() {
                        ui.title = command.title.clone();
                    }
                    ui
                });
                println!("command id: {}", command.id);
                targets.push(CommandTarget {
                    bundle: info.bundle.clone(),
                    plugin_id: meta.id().to_string(),
                    plugin_name: meta.display_name().to_string(),
                    root: meta.root.clone(),
                    command_id: command.id.clone(),
                    title: command.title.clone(),
                    description: command.description.clone(),
                    ui,
                });
            }
        }
        targets
    }

    /// 懒加载：第一次执行命令时才建运行时（插件顶层的副作用不该在启动时就跑）。
    pub(crate) fn ensure_loaded(&mut self, target: &CommandTarget) -> Result<(), String> {
        let already = self
            .loader
            .get_plugin_by_bundle(&target.bundle)
            .map(|plugin| plugin.is_load())
            .unwrap_or(false);
        if already {
            return Ok(());
        }
        self.loader
            .load_plugin_by_bundle(&target.bundle)
            .map_err(|e| e.to_string())
    }

    /// 执行入口的 `Arc` 副本（RunService 需要它才能在工作线程里调命令）。
    pub(crate) fn store(&self) -> Arc<PluginStore> {
        self.store.clone()
    }
}

fn describe(error: &RegisterPluginError) -> String {
    match error {
        // plux 的外层 Display 只有"manager 报错"，真正原因在里层。
        RegisterPluginError::RegisterPluginByManager(inner) => inner.to_string(),
        RegisterPluginError::BundleFromFailed(inner) => inner.to_string(),
        RegisterPluginError::NotFound => "插件目录不存在".to_string(),
        RegisterPluginError::UnknownManagerFormat(format) => {
            format!("未知插件格式 `{format}`（目录名必须以 `.js` 结尾）")
        }
        RegisterPluginError::AlreadyExistsIDAndVersion(id, version) => {
            format!("同 id 同版本已注册：{id} v{version}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console_channel::ConsoleChannel;
    use crate::host::HostRegistry;
    use crate::modules::extensions::js_env::BoltJsEnv;
    use crate::modules::extensions::manifest;
    use crate::test_support::TempDir;
    use plux_js_manager::{CancelToken, InvokeRequest, JsError, MANIFEST_FILE};

    fn host(tmp: &TempDir) -> ExtensionsHost {
        let mut registry = HostRegistry::new();
        crate::host::modules::register_all(&mut registry);
        let (console, _rx) = ConsoleChannel::new();
        let env = Arc::new(BoltJsEnv {
            registry: Arc::new(registry),
            console,
        });
        ExtensionsHost::new(&tmp.0, env)
    }

    fn write_plugin(tmp: &TempDir, dir: &str, manifest: &str, entry: &str) {
        let root = tmp.0.join(dir);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(MANIFEST_FILE), manifest).unwrap();
        std::fs::write(root.join("index.js"), entry).unwrap();
    }

    #[test]
    fn demo_plugin_contributes_script_and_ui_commands() {
        let tmp = TempDir::new("host-demo");
        manifest::ensure_demo_plugin(&tmp.0).expect("write demo plugin");

        let mut host = host(&tmp);
        let errors = host.refresh();
        assert!(errors.is_empty(), "{errors:?}");

        let commands = host.commands();
        assert_eq!(commands.len(), 2, "{commands:?}");
        let hello = commands
            .iter()
            .find(|command| command.command_id == "hello")
            .expect("hello command");
        assert!(!hello.is_ui());
        assert_eq!(hello.item_id(), "ext.demo.hello");
        let panel = commands
            .iter()
            .find(|command| command.command_id == "panel")
            .expect("panel command");
        assert_eq!(
            panel.ui.as_ref().map(|ui| ui.title.as_str()),
            Some("演示面板")
        );

        host.ensure_loaded(hello).expect("demo should load");
        let output = host
            .store()
            .invoke(
                "demo",
                "1.0.0",
                InvokeRequest::new("hello", CancelToken::new()),
            )
            .expect("hello should run");
        assert!(
            output.unwrap_or_default().contains("demo v1.0.0"),
            "unexpected output"
        );
    }

    #[test]
    fn plugins_can_import_host_modules() {
        let tmp = TempDir::new("host-modules");
        write_plugin(
            &tmp,
            "probe-v1.0.0.js",
            r#"{ "id": "probe", "version": "1.0.0", "permissions": ["env"], "commands": [{ "id": "home", "title": "Home" }] }"#,
            "import { env } from 'bolt';\nexport function home() { return env('HOME') ?? 'unset'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("probe should load");

        let output = host
            .store()
            .invoke(
                "probe",
                "1.0.0",
                InvokeRequest::new("home", CancelToken::new()),
            )
            .expect("home should run");
        assert!(output.is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn undeclared_permissions_throw_at_call_time() {
        // fail-closed：清单没声明 fs，import 不炸，调用 writeFile 抛出点名报错。
        let tmp = TempDir::new("host-perm-denied");
        write_plugin(
            &tmp,
            "guard-v1.0.0.js",
            r#"{ "id": "guard", "version": "1.0.0", "commands": [{ "id": "write", "title": "Write" }] }"#,
            "import { writeFile } from 'bolt';\nexport function write() { writeFile('/tmp/nope', 'x'); }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("guard should load");

        let error = host
            .store()
            .invoke(
                "guard",
                "1.0.0",
                InvokeRequest::new("write", CancelToken::new()),
            )
            .expect_err("denied call must throw");
        match error {
            JsError::Script(message) => {
                assert!(message.contains("未声明权限"), "{message}");
                assert!(message.contains("fs"), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn broken_plugin_is_reported_without_blocking_the_others() {
        let tmp = TempDir::new("host-broken");
        write_plugin(
            &tmp,
            "bad-v1.0.0.js",
            r#"{ "id": "bad", "version": "1.0.0", "commands": "nope" }"#,
            "",
        );
        write_plugin(
            &tmp,
            "good-v1.0.0.js",
            r#"{ "id": "good", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'ok'; }",
        );

        let mut host = host(&tmp);
        let errors = host.refresh();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("bad-v1.0.0.js"), "{errors:?}");

        let commands = host.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].plugin_id, "good");
    }

    #[test]
    fn refresh_is_idempotent() {
        let tmp = TempDir::new("host-refresh");
        write_plugin(
            &tmp,
            "one-v1.0.0.js",
            r#"{ "id": "one", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() {}",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        // 第二次扫描不该把已注册的插件当成错误报出来。
        assert!(host.refresh().is_empty());
        assert_eq!(host.commands().len(), 1);
    }
}
