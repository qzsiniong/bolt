use super::script::{Language, Script};
use std::path::{Path, PathBuf};

/// Returns the scripts directory: `~/Library/Application Support/bolt/scripts/`.
pub fn scripts_dir() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
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
        Language::TypeScript => {
            "// new TypeScript script\nconst greet = (name: string): string => `hello ${name}`;\nconsole.log(greet('world'));\n"
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn list_scripts_filters_and_sorts_by_name() {
        let tmp = TempDir::new("store-list");
        tmp.write("zeta.ts", "// z");
        tmp.write("alpha.js", "// a");
        tmp.write("mid.json", "{}"); // 非脚本扩展名
        tmp.write("notes.md", "text");
        std::fs::create_dir_all(tmp.0.join("subdir")).unwrap(); // 目录跳过

        let scripts = list_scripts(&tmp.0);
        let names: Vec<&str> = scripts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert_eq!(scripts[0].language, Language::JavaScript);
        assert_eq!(scripts[1].language, Language::TypeScript);
    }

    #[test]
    fn list_scripts_on_missing_dir_is_empty() {
        let tmp = TempDir::new("store-list-missing");
        assert!(list_scripts(&tmp.0.join("nope")).is_empty());
    }

    #[test]
    fn create_script_writes_a_starter_and_rejects_bad_names() {
        let tmp = TempDir::new("store-create");

        let script = create_script(&tmp.0, "hello-world_1", Language::TypeScript).unwrap();
        assert_eq!(script.file_name, "hello-world_1.ts");
        assert!(script.content.contains("hello ${name}"));

        // 重复创建被拒。
        let err = create_script(&tmp.0, "hello-world_1", Language::TypeScript).unwrap_err();
        assert!(err.contains("already exists"), "{err}");

        // 非法名字：空串、空格、路径分隔符、点（`is_alphanumeric` 是 Unicode
        // 语义，中文等字母数字字符是允许的）。
        for bad in ["", "with space", "a/b", "..", "a.b"] {
            assert!(
                create_script(&tmp.0, bad, Language::JavaScript).is_err(),
                "`{bad}` should be rejected"
            );
        }
    }

    #[test]
    fn save_read_delete_roundtrip() {
        let tmp = TempDir::new("store-roundtrip");
        create_script(&tmp.0, "demo", Language::JavaScript).unwrap();

        save_script(&tmp.0, "demo.js", "console.log('v2');").unwrap();
        let script = read_script(&tmp.0, "demo.js").unwrap();
        assert_eq!(script.content, "console.log('v2');");

        delete_script(&tmp.0, "demo.js").unwrap();
        assert!(!tmp.0.join("demo.js").exists());
        // 再删一次报"不存在"。
        let err = delete_script(&tmp.0, "demo.js").unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn save_cannot_escape_the_dir_check_is_caller_duty() {
        // `save_script` 对 file_name 不做校验（调用方保证），但 read 对
        // 不存在的文件要报错而不是 panic。
        let tmp = TempDir::new("store-read-missing");
        assert!(read_script(&tmp.0, "ghost.js").is_err());
    }
}
