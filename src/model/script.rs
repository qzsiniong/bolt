use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    JavaScript,
    TypeScript,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("js") => Some(Self::JavaScript),
            Some("ts") => Some(Self::TypeScript),
            _ => None,
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::JavaScript => "js",
            Self::TypeScript => "ts",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Script {
    pub name: String,
    pub file_name: String,
    pub path: PathBuf,
    pub language: Language,
    pub content: String,
}

impl Script {
    pub fn read(path: &Path) -> Result<Self, String> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "invalid file name".to_string())?
            .to_string();
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "invalid file stem".to_string())?
            .to_string();
        let language = Language::from_path(path).ok_or_else(|| "unknown extension".to_string())?;
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Ok(Self {
            name,
            file_name,
            path: path.to_path_buf(),
            language,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_from_extension() {
        assert_eq!(
            Language::from_path(Path::new("a.js")),
            Some(Language::JavaScript)
        );
        assert_eq!(
            Language::from_path(Path::new("a.ts")),
            Some(Language::TypeScript)
        );
        assert_eq!(Language::from_path(Path::new("a.json")), None);
        assert_eq!(Language::from_path(Path::new("a")), None);
        assert_eq!(Language::JavaScript.extension(), "js");
        assert_eq!(Language::TypeScript.as_str(), "typescript");
    }

    #[test]
    fn script_read_extracts_name_stem_and_content() {
        let tmp = crate::test_support::TempDir::new("script-read");
        let path = tmp.0.join("my.tool.js");
        std::fs::write(&path, "// hi").unwrap();

        let script = Script::read(&path).unwrap();
        assert_eq!(script.name, "my.tool"); // file_stem：点号属于名字
        assert_eq!(script.file_name, "my.tool.js");
        assert_eq!(script.language, Language::JavaScript);
        assert_eq!(script.content, "// hi");
    }

    #[test]
    fn script_read_rejects_unknown_extension() {
        let tmp = crate::test_support::TempDir::new("script-read-bad");
        let path = tmp.0.join("note.md");
        std::fs::write(&path, "x").unwrap();
        assert_eq!(Script::read(&path).unwrap_err(), "unknown extension");
    }
}
