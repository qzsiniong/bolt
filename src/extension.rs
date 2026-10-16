//! External extensions: command bundles living next to the scripts directory.
//!
//! Layout: `<data_dir>/scripter/extensions/<id>/extension.json` plus a JS/TS
//! entry file. Each extension contributes one or more commands to the
//! launcher; running a command executes the entry file with the same host
//! APIs as a regular script.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::script::Language;

pub const MANIFEST_FILE: &str = "extension.json";
pub const DEFAULT_ENTRY: &str = "index.js";

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
    /// Kept for diagnostics and future per-extension resources (assets etc.).
    #[allow(dead_code)]
    pub dir: PathBuf,
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
    Ok(Extension {
        dir: dir.to_path_buf(),
        manifest,
        entry,
    })
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

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "scripter-ext-test-{}-{}",
                name,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

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
}
