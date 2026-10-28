//! `UsageStorage` 的 JSON 文件实现。
//!
//! 只负责「一个文件 ↔ 一份 [`UsageData`]」，不做迁移、不决定路径：使用记录的
//! 文件位置与老格式升级由调用方（`app::usage`）负责。

use std::fs;
use std::path::PathBuf;

use super::storage::{StorageError, UsageData, UsageStorage};

/// 把使用记录存成一个 JSON 文件。文件内容是 `{ item id: [时间戳…] }`。
pub(crate) struct FileStorage {
    path: PathBuf,
}

impl FileStorage {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl UsageStorage for FileStorage {
    fn load(&self) -> Result<UsageData, StorageError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            // 首次使用（或记录文件被清理）不是错误：空表即可，下一次 record
            // 会把文件建出来。其它读失败才上报。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(UsageData::new()),
            Err(e) => {
                return Err(StorageError::LoadFailed(format!(
                    "{}: {e}",
                    self.path.display()
                )));
            }
        };
        serde_json::from_str(&text)
            .map_err(|e| StorageError::LoadFailed(format!("{}: {e}", self.path.display())))
    }

    fn save(&self, data: &UsageData) -> Result<(), StorageError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| {
                StorageError::SaveFailed(format!("创建目录 {} 失败：{e}", dir.display()))
            })?;
        }
        let json = serde_json::to_string_pretty(data)
            .map_err(|e| StorageError::SaveFailed(format!("序列化失败：{e}")))?;
        fs::write(&self.path, json)
            .map_err(|e| StorageError::SaveFailed(format!("{}: {e}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn sample() -> UsageData {
        let mut data = UsageData::new();
        data.insert(
            "apps.open:arc".to_string(),
            vec!["2026-01-01T00:00:00+00:00".to_string()],
        );
        data
    }

    #[test]
    fn roundtrips_through_a_file() {
        let temp = TempDir::new("file-storage");
        let path = temp.0.join("item-usage.json");
        let storage = FileStorage::new(&path);

        storage.save(&sample()).expect("save");
        let loaded = storage.load().expect("load");
        assert_eq!(loaded, sample());
    }

    #[test]
    fn missing_file_is_an_empty_dataset() {
        let temp = TempDir::new("file-storage-missing");
        let storage = FileStorage::new(temp.0.join("nope.json"));
        assert!(storage.load().expect("load").is_empty());
    }

    #[test]
    fn corrupt_file_is_reported() {
        let temp = TempDir::new("file-storage-corrupt");
        let path = temp.0.join("item-usage.json");
        std::fs::write(&path, b"{ not json").expect("write");
        assert!(FileStorage::new(&path).load().is_err());
    }

    #[test]
    fn save_creates_the_parent_directory() {
        let temp = TempDir::new("file-storage-dirs");
        let path = temp.0.join("nested/deeper/item-usage.json");
        FileStorage::new(&path).save(&sample()).expect("save");
        assert!(path.is_file());
    }
}
