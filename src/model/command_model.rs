//! Unified command model for the launcher: built-in commands, one command per
//! script, one command per external extension command, and one command per UI
//! extension (gpui-shell) — all grouped for the command palette.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;

use super::{extension, script_store};

/// What confirming a launcher command does.
#[derive(Clone, Debug)]
pub enum CommandAction {
    /// Open (or focus) the script manager window.
    OpenManager,
    /// Open the manager and start its new-script flow.
    NewScript,
    /// Run a script file from the scripts directory.
    RunScript(PathBuf),
    /// Run an extension's entry file.
    RunExtension { entry: PathBuf },
    /// Open a UI extension (gpui-shell) in its own native window.
    OpenExtensionUi { ext: extension::UiExtension },
}

/// One row in the launcher.
#[derive(Clone, Debug)]
pub struct AppCommand {
    /// Globally unique id: `builtin.*` / `script.<file>` / `ext.<id>.<cmd>`.
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: IconName,
    pub action: CommandAction,
}

/// Commands grouped for the palette; `IndexPath { section, row }` from the
/// command component resolves through [`Self::resolve`].
#[derive(Clone, Debug, Default)]
pub struct CommandGroups {
    pub groups: Vec<(String, Vec<AppCommand>)>,
}

impl CommandGroups {
    pub fn resolve(&self, section: usize, row: usize) -> Option<&AppCommand> {
        self.groups.get(section)?.1.get(row)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.groups.iter().map(|(_, items)| items.len()).sum()
    }
}

/// Build the launcher's command list from the current scripts and extensions
/// on disk. Broken extension directories come back as errors so the caller can
/// surface them instead of failing the whole list.
pub fn collect_commands(
    scripts_dir: &Path,
    extensions_dir: &Path,
) -> (CommandGroups, Vec<extension::ExtensionError>) {
    let mut groups = Vec::new();

    let builtin = vec![
        AppCommand {
            id: "builtin.open-manager".to_string(),
            title: "脚本管理".to_string(),
            subtitle: Some("打开脚本管理器窗口".to_string()),
            icon: IconName::FolderCog,
            action: CommandAction::OpenManager,
        },
        AppCommand {
            id: "builtin.new-script".to_string(),
            title: "新建脚本".to_string(),
            subtitle: Some("在脚本管理器中创建新脚本".to_string()),
            icon: IconName::FilePlus,
            action: CommandAction::NewScript,
        },
    ];
    groups.push(("内置".to_string(), builtin));

    let scripts: Vec<AppCommand> = script_store::list_scripts(scripts_dir)
        .into_iter()
        .map(|script| AppCommand {
            id: format!("script.{}", script.file_name),
            title: script.name.clone(),
            subtitle: Some(script.file_name.clone()),
            icon: IconName::FileCode,
            action: CommandAction::RunScript(script.path),
        })
        .collect();
    if !scripts.is_empty() {
        groups.push(("脚本".to_string(), scripts));
    }

    let (extensions, mut errors) = extension::list_extensions(extensions_dir);
    for ext in extensions {
        let commands: Vec<AppCommand> = extension::commands_of(&ext)
            .into_iter()
            .map(|cmd| AppCommand {
                id: format!("ext.{}.{}", ext.manifest.id, cmd.id),
                title: cmd.title,
                subtitle: cmd.subtitle,
                icon: IconName::Puzzle,
                action: CommandAction::RunExtension {
                    entry: ext.entry.clone(),
                },
            })
            .collect();
        if !commands.is_empty() {
            groups.push((ext.manifest.display_name().to_string(), commands));
        }
    }

    // UI extensions: one command each, opening the extension's own window.
    let (ui_extensions, ui_errors) = extension::list_ui_extensions(extensions_dir);
    errors.extend(ui_errors);
    for ext in ui_extensions {
        let command = AppCommand {
            id: format!("ui.{}", ext.id()),
            title: ext.name().to_string(),
            subtitle: Some(format!("gpui-shell · {}", ext.id())),
            icon: IconName::AppWindow,
            action: CommandAction::OpenExtensionUi { ext: ext.clone() },
        };
        groups.push((ext.name().to_string(), vec![command]));
    }

    (CommandGroups { groups }, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn empty_dirs_yield_only_builtin() {
        let tmp = TempDir::new("builtin-only");
        let (groups, errors) = collect_commands(&tmp.0.join("scripts"), &tmp.0.join("extensions"));
        assert!(errors.is_empty());
        assert_eq!(groups.groups.len(), 1);
        assert_eq!(groups.groups[0].0, "内置");
        assert_eq!(groups.len(), 2);
        assert!(matches!(
            groups.resolve(0, 0).map(|c| &c.action),
            Some(CommandAction::OpenManager)
        ));
        assert!(groups.resolve(1, 0).is_none());
    }

    #[test]
    fn scripts_and_extensions_become_commands() {
        let tmp = TempDir::new("full");
        tmp.write("scripts/alpha.js", "console.log(1);");
        tmp.write("scripts/beta.ts", "console.log(2);");
        tmp.write(
            "extensions/demo/extension.json",
            r#"{ "id": "demo", "commands": [{ "id": "go", "title": "Go" }] }"#,
        );
        tmp.write("extensions/demo/index.js", "");

        let (groups, errors) = collect_commands(&tmp.0.join("scripts"), &tmp.0.join("extensions"));
        assert!(errors.is_empty());
        assert_eq!(groups.groups.len(), 3);
        assert_eq!(groups.groups[1].0, "脚本");
        assert_eq!(groups.groups[1].1.len(), 2);
        assert_eq!(groups.groups[2].0, "demo");
        match groups.resolve(1, 0).map(|c| &c.action) {
            Some(CommandAction::RunScript(path)) => {
                assert!(path.ends_with("alpha.js"));
            }
            other => panic!("expected RunScript, got {other:?}"),
        }
        match groups.resolve(2, 0).map(|c| &c.action) {
            Some(CommandAction::RunExtension { entry }) => {
                assert!(entry.ends_with("index.js"));
            }
            other => panic!("expected RunExtension, got {other:?}"),
        }
    }

    #[test]
    fn ui_extensions_get_their_own_group_and_action() {
        let tmp = TempDir::new("ui-command");
        tmp.write(
            "extensions/ui/extension.json",
            r#"{ "id": "ui", "name": "UI 示例", "kind": "ui", "entry": "main.js" }"#,
        );
        tmp.write("extensions/ui/main.js", "");

        let (groups, errors) = collect_commands(&tmp.0.join("scripts"), &tmp.0.join("extensions"));
        assert!(errors.is_empty());
        assert_eq!(groups.groups.len(), 2);
        assert_eq!(groups.groups[1].0, "UI 示例");
        assert_eq!(groups.groups[1].1.len(), 1);
        assert_eq!(groups.groups[1].1[0].id, "ui.ui");
        match groups.resolve(1, 0).map(|c| &c.action) {
            Some(CommandAction::OpenExtensionUi { ext }) => {
                assert_eq!(ext.id(), "ui");
                assert_eq!(ext.entry(), "main.js");
                assert!(ext.root.ends_with("ui"));
            }
            other => panic!("expected OpenExtensionUi, got {other:?}"),
        }
    }

    #[test]
    fn broken_ui_manifest_is_reported_in_errors() {
        let tmp = TempDir::new("ui-command-broken");
        // kind 是 ui 但没写 main.js 入口 → UI 侧报错。
        tmp.write("extensions/bad/extension.json", r#"{ "id": "bad", "kind": "ui" }"#);

        let (groups, errors) = collect_commands(&tmp.0.join("scripts"), &tmp.0.join("extensions"));
        assert_eq!(groups.groups.len(), 1);
        assert_eq!(errors.len(), 1);
    }
}
