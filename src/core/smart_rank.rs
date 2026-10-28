//! 按时间衰减的「智能排序」引擎。
//!
//! 每个 item 记一串使用事件；它当前的分数是这些事件的指数衰减之和：
//! `Σ exp(-λ · 距今天数)`，只统计评分窗口内的事件。于是
//!
//! - 近期用过的排在很久以前用过的前面；
//! - 高频使用的排在偶尔使用的前面；
//! - 长期不用会自然滑出窗口，不需要人工清理（`prune` 负责真正删掉老数据）。
//!
//! 引擎只依赖 [`UsageStorage`] 抽象，不关心数据存在哪里；路径与「分数怎么
//! 变成 UI 顺序」由调用方决定（见 `app::usage`）。

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use super::storage::{StorageError, UsageStorage};

/// 一天有多少秒，用于把时间差换算成天。
const SECONDS_PER_DAY: f64 = 86_400.0;

/// 排序配置。
#[derive(Debug, Clone)]
pub(crate) struct RankConfig {
    /// 衰减系数，越大遗忘越快（推荐 0.05~0.3）。
    pub(crate) lambda: f64,
    /// 事件最大保留天数（超出的事件在写入 / `prune` 时删除）。
    pub(crate) max_age_days: i64,
    /// 每个 item 最多保留的事件数（只保留最近的这么多条）。
    pub(crate) max_events_per_item: usize,
    /// 评分时只看最近 N 天的事件。
    pub(crate) score_window_days: i64,
}

impl Default for RankConfig {
    fn default() -> Self {
        Self {
            lambda: 0.1,
            max_age_days: 90,
            max_events_per_item: 200,
            score_window_days: 30,
        }
    }
}

/// 智能排序引擎：无内部可变状态，可以按需现建（每次操作读一次存储）。
pub(crate) struct SmartRank {
    config: RankConfig,
    storage: Box<dyn UsageStorage>,
}

impl SmartRank {
    pub(crate) fn new(config: RankConfig, storage: Box<dyn UsageStorage>) -> Self {
        Self { config, storage }
    }

    /// 记录一次使用：追加「现在」的事件，并把该 item 的事件裁到保留策略内。
    ///
    /// 历史读不出来（旧格式 / 文件损坏）时从空表重新开始：使用统计是缓存，
    /// 不该因为它挡住「记录」本身（否则永远自愈不了）。
    pub(crate) fn record(&self, item: &str) -> Result<(), StorageError> {
        let mut data = self.storage.load().unwrap_or_default();
        let events = data.entry(item.to_string()).or_default();
        events.push(Utc::now().to_rfc3339());
        Self::prune_events(events, &self.config);
        self.storage.save(&data)
    }

    /// 所有 item 的当前分数（评分窗口内没有任何事件 → 0）。
    pub(crate) fn scores(&self) -> Result<HashMap<String, f64>, StorageError> {
        let data = self.storage.load()?;
        let now = Utc::now();
        Ok(data
            .iter()
            .map(|(item, events)| (item.clone(), self.score_events(events, now)))
            .collect())
    }

    /// 全量裁剪：删掉过期 / 超量事件与清空后的 item（可定期调用）。
    pub(crate) fn prune(&self) -> Result<(), StorageError> {
        let mut data = self.storage.load()?;
        if data.is_empty() {
            return Ok(());
        }
        for events in data.values_mut() {
            Self::prune_events(events, &self.config);
        }
        data.retain(|_, events| !events.is_empty());
        self.storage.save(&data)
    }

    /// 单组事件的衰减分数。
    fn score_events(&self, events: &[String], now: DateTime<Utc>) -> f64 {
        let window_start = now - Duration::days(self.config.score_window_days);
        events
            .iter()
            .filter_map(|ts| ts.parse::<DateTime<Utc>>().ok())
            .filter(|at| *at > window_start)
            .map(|at| {
                let days_ago = (now - at).num_seconds() as f64 / SECONDS_PER_DAY;
                (-self.config.lambda * days_ago).exp()
            })
            .sum()
    }

    /// 事件保留策略：先删超过 `max_age_days` 的，再从最旧的开始截到
    /// `max_events_per_item`（事件按发生顺序追加，所以头部最旧）。
    fn prune_events(events: &mut Vec<String>, config: &RankConfig) {
        let cutoff = Utc::now() - Duration::days(config.max_age_days);
        events.retain(|ts| {
            ts.parse::<DateTime<Utc>>()
                .map(|at| at > cutoff)
                .unwrap_or(false)
        });
        if events.len() > config.max_events_per_item {
            let excess = events.len() - config.max_events_per_item;
            events.drain(0..excess);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::core::storage::UsageData;

    /// 内存存储：排序逻辑的测试不碰磁盘。
    #[derive(Clone, Default)]
    struct MemStorage(Rc<RefCell<UsageData>>);

    impl MemStorage {
        fn with(data: UsageData) -> Self {
            Self(Rc::new(RefCell::new(data)))
        }

        fn events(&self, item: &str) -> Vec<String> {
            self.0.borrow().get(item).cloned().unwrap_or_default()
        }
    }

    impl UsageStorage for MemStorage {
        fn load(&self) -> Result<UsageData, StorageError> {
            Ok(self.0.borrow().clone())
        }

        fn save(&self, data: &UsageData) -> Result<(), StorageError> {
            *self.0.borrow_mut() = data.clone();
            Ok(())
        }
    }

    /// 相对现在 N 天前的时间戳。
    fn days_ago(days: i64) -> String {
        (Utc::now() - Duration::days(days)).to_rfc3339()
    }

    fn engine_with(config: RankConfig, items: &[(&str, &[i64])]) -> (SmartRank, MemStorage) {
        let mut data = UsageData::new();
        for (item, days) in items {
            data.insert(
                (*item).to_string(),
                days.iter().map(|d| days_ago(*d)).collect(),
            );
        }
        let storage = MemStorage::with(data);
        (SmartRank::new(config, Box::new(storage.clone())), storage)
    }

    #[test]
    fn recent_use_outranks_older_use() {
        let (engine, _) = engine_with(RankConfig::default(), &[("old", &[20]), ("recent", &[1])]);
        let scores = engine.scores().expect("scores");
        assert!(scores["recent"] > scores["old"]);
        assert!(scores["old"] > 0.0);
    }

    #[test]
    fn repeated_use_outranks_a_single_use_at_the_same_age() {
        let (engine, _) = engine_with(
            RankConfig::default(),
            &[("frequent", &[0, 1, 2]), ("once", &[1])],
        );
        let scores = engine.scores().expect("scores");
        assert!(scores["frequent"] > scores["once"]);
    }

    #[test]
    fn events_outside_the_score_window_count_as_zero() {
        let (engine, _) = engine_with(RankConfig::default(), &[("stale", &[45])]);
        assert_eq!(engine.scores().expect("scores")["stale"], 0.0);
    }

    #[test]
    fn record_appends_a_fresh_event() {
        let (engine, storage) = engine_with(RankConfig::default(), &[("x", &[10])]);
        engine.record("x").expect("record");
        let events = storage.events("x");
        assert_eq!(events.len(), 2);
        // 刚记的一次让分数明显上升。
        assert!(engine.scores().expect("scores")["x"] > 1.0);
    }

    #[test]
    fn record_drops_events_past_the_retention_window() {
        let (engine, storage) = engine_with(RankConfig::default(), &[("x", &[120])]);
        engine.record("x").expect("record");
        assert_eq!(storage.events("x").len(), 1);
    }

    #[test]
    fn prune_caps_events_and_keeps_the_newest() {
        let config = RankConfig {
            max_events_per_item: 2,
            ..RankConfig::default()
        };
        let (engine, storage) = engine_with(config, &[("x", &[2, 1, 0])]);
        engine.prune().expect("prune");

        let events = storage.events("x");
        assert_eq!(events.len(), 2);
        // 保留的是最近两条（2 天前那条被丢掉）。
        assert!(!events.contains(&days_ago(2)));
    }

    #[test]
    fn prune_removes_items_whose_events_all_expired() {
        let (engine, storage) = engine_with(RankConfig::default(), &[("gone", &[120])]);
        engine.prune().expect("prune");
        assert!(storage.0.borrow().is_empty());
    }
}
