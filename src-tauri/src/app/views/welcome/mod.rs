//! 欢迎页只提供入口。最近执行和保存的查询沿用历史载入计划，不执行 SQL。

use crate::app::views::history::{plan_from_run, plan_from_saved, LoadPlan};
use crate::query::{HistoryEntry, SavedQuery};

pub const RECENT_LIMIT: usize = 6;
pub const SAVED_LIMIT: usize = 4;

#[derive(Clone, Debug)]
pub struct WelcomePage {
    pub has_connections: bool,
}

impl WelcomePage {
    pub fn recent<'a>(&self, runs: &'a [HistoryEntry]) -> &'a [HistoryEntry] {
        let end = runs.len().min(RECENT_LIMIT);
        &runs[..end]
    }

    pub fn saved<'a>(&self, saved: &'a [SavedQuery]) -> &'a [SavedQuery] {
        let end = saved.len().min(SAVED_LIMIT);
        &saved[..end]
    }

    pub fn open_recent(&self, entry: &HistoryEntry, known: &std::collections::BTreeSet<String>) -> LoadPlan {
        plan_from_run(entry, known)
    }

    pub fn open_saved(&self, item: &SavedQuery, known: &std::collections::BTreeSet<String>) -> LoadPlan {
        plan_from_saved(item, known)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_reopen_does_not_execute() {
        let page = WelcomePage { has_connections: true };
        let entry = HistoryEntry {
            id: "1".into(),
            sql: "select '你好'".into(),
            connection_id: "c".into(),
            connection_name: "本地".into(),
            database_name: "main".into(),
            engine: "sqlite".into(),
            started_at: "2026-11-07T00:00:00Z".into(),
            finished_at: None,
            status: "ok".into(),
            elapsed_ms: None,
            summary: String::new(),
        };
        let mut known = std::collections::BTreeSet::new();
        known.insert("c".into());
        let plan = page.open_recent(&entry, &known);
        assert!(!plan.execute);
        assert_eq!(plan.sql, "select '你好'");
    }
}
