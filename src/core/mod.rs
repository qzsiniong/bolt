//! Core：与具体功能无关的通用基础设施，可单独测试。
//!
//! - [`storage`]：使用记录的数据形状、错误与存储抽象；
//! - [`file_storage`]：把使用记录落到 JSON 文件的 [`storage::UsageStorage`] 实现；
//! - [`smart_rank`]：按时间衰减打分的排序引擎。
//!
//! 依赖方向是单向的：`smart_rank` → `storage` ← `file_storage`。文件位置、
//! 老格式迁移与「分数怎么变成 UI 顺序」都由调用方决定（见 `app::usage`）。

pub(crate) mod file_storage;
pub(crate) mod smart_rank;
pub(crate) mod storage;
