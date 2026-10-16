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
