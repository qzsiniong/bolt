use crate::script::{Language, Script};
use std::path::{Path, PathBuf};

/// Returns the scripts directory: `~/Library/Application Support/scripter/scripts/`.
pub fn scripts_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("scripter");
    path.push("scripts");
    path
}

pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create scripts dir: {e}"))
}

pub fn list_scripts(dir: &Path) -> Vec<Script> {
    let mut scripts = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return scripts;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if Language::from_path(&path).is_none() {
            continue;
        }
        if let Ok(script) = Script::read(&path) {
            scripts.push(script);
        }
    }
    scripts.sort_by(|a, b| a.name.cmp(&b.name));
    scripts
}

pub fn read_script(dir: &Path, file_name: &str) -> Result<Script, String> {
    let path = dir.join(file_name);
    Script::read(&path)
}

pub fn create_script(dir: &Path, name: &str, language: Language) -> Result<Script, String> {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        // ok
    } else {
        return Err("name must be alphanumeric/underscore/hyphen".into());
    }
    let file_name = format!("{}.{}", name, language.extension());
    let path = dir.join(&file_name);
    if path.exists() {
        return Err(format!("file `{file_name}` already exists"));
    }
    let starter = match language {
        Language::JavaScript => "// new JavaScript script\nconsole.log('hello');\n",
        Language::TypeScript => "// new TypeScript script\nconst greet = (name: string): string => `hello ${name}`;\nconsole.log(greet('world'));\n",
    };
    std::fs::write(&path, starter).map_err(|e| e.to_string())?;
    Script::read(&path)
}

pub fn delete_script(dir: &Path, file_name: &str) -> Result<(), String> {
    let path = dir.join(file_name);
    if !path.exists() {
        return Err(format!("file `{file_name}` not found"));
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

pub fn save_script(dir: &Path, file_name: &str, content: &str) -> Result<(), String> {
    let path = dir.join(file_name);
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

pub fn rename_script(dir: &Path, old: &str, new_name: &str, language: Language) -> Result<Script, String> {
    let old_path = dir.join(old);
    let new_file = format!("{}.{}", new_name, language.extension());
    let new_path = dir.join(&new_file);
    if new_path.exists() && old_path != new_path {
        return Err(format!("file `{new_file}` already exists"));
    }
    std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())?;
    Script::read(&new_path)
}
