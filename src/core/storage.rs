//! 使用记录的存储抽象：数据形状、错误类型、读写接口。
//!
//! 排序引擎（[`super::smart_rank`]）只依赖这里的 [`UsageStorage`]，**不关心**
//! 数据存在哪里；具体实现见 [`super::file_storage`]（JSON 文件），测试里可以
//! 换成内存实现。
//!
//! 数据形状是「item id → 使用事件的时间戳列表」：每次使用追加一条，保留原始
//! 时间点，让排序按「多久以前用过、用过几次」一起衰减，而不是只记一个总数。

use std::collections::HashMap;

/// item id → 使用事件（RFC 3339 时间戳，按发生顺序追加）。
pub(crate) type UsageData = HashMap<String, Vec<String>>;

/// 存储层错误。
#[derive(Debug)]
pub(crate) enum StorageError {
    LoadFailed(String),
    SaveFailed(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::LoadFailed(s) => write!(f, "读取使用记录失败：{s}"),
            StorageError::SaveFailed(s) => write!(f, "写入使用记录失败：{s}"),
        }
    }
}

impl std::error::Error for StorageError {}

/// 存储层抽象：排序引擎通过它读写全部使用记录。
pub(crate) trait UsageStorage {
    fn load(&self) -> Result<UsageData, StorageError>;
    fn save(&self, data: &UsageData) -> Result<(), StorageError>;
}
