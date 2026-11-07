//! 历史和保存的查询只恢复草稿。这里没有任何执行 SQL 的入口。

use crate::application::AppServices;
use crate::query::{HistoryEntry, RenameRequest, SaveRequest, SavedQuery};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySection {
    Runs,
    Saved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadPlan {
    pub sql: String,
    pub connection_id: Option<String>,
    pub connection_name: String,
    pub database: Option<String>,
    pub engine: String,
    pub needs_target: bool,
    pub execute: bool,
    pub notice_key: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub struct HistoryPage {
    pub section: HistorySection,
    pub runs: Vec<HistoryEntry>,
    pub saved: Vec<SavedQuery>,
    pub filter_text: String,
    pub filter_connection: String,
    pub filter_status: String,
    pub selected_run: Option<String>,
    pub selected_saved: Option<String>,
    pub save_name: String,
    pub message_key: Option<&'static str>,
    pub confirm_clear: bool,
    pub confirm_delete: bool,
    pub loads: u32,
    pub executions: u32,
}

impl Default for HistoryPage {
    fn default() -> Self {
        Self {
            section: HistorySection::Runs,
            runs: Vec::new(),
            saved: Vec::new(),
            filter_text: String::new(),
            filter_connection: String::new(),
            filter_status: String::new(),
            selected_run: None,
            selected_saved: None,
            save_name: String::new(),
            message_key: None,
            confirm_clear: false,
            confirm_delete: false,
            loads: 0,
            executions: 0,
        }
    }
}

impl HistoryPage {
    pub fn filtered_runs(&self) -> Vec<&HistoryEntry> {
        let needle = self.filter_text.trim().to_ascii_lowercase();
        self.runs
            .iter()
            .filter(|entry| {
                if !self.filter_connection.is_empty() && entry.connection_id != self.filter_connection {
                    return false;
                }
                if !self.filter_status.is_empty() && entry.status != self.filter_status {
                    return false;
                }
                if needle.is_empty() {
                    return true;
                }
                format!("{} {}", entry.sql, entry.connection_name).to_ascii_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn filtered_saved(&self) -> Vec<&SavedQuery> {
        let needle = self.filter_text.trim().to_ascii_lowercase();
        self.saved.iter().filter(|item| needle.is_empty() || item.name.to_ascii_lowercase().contains(&needle) || item.sql.to_ascii_lowercase().contains(&needle)).collect()
    }

    pub fn plan_run(&self, known_ids: &std::collections::BTreeSet<String>) -> Option<LoadPlan> {
        let id = self.selected_run.as_deref()?;
        let entry = self.runs.iter().find(|entry| entry.id == id)?;
        Some(plan_from_run(entry, known_ids))
    }

    pub fn plan_saved(&self, known_ids: &std::collections::BTreeSet<String>) -> Option<LoadPlan> {
        let id = self.selected_saved.as_deref()?;
        let item = self.saved.iter().find(|item| item.id == id)?;
        Some(plan_from_saved(item, known_ids))
    }

    pub fn filter_selected_connection(&mut self) {
        let Some(id) = self.selected_run.clone() else { return };
        let Some(entry) = self.runs.iter().find(|entry| entry.id == id) else { return };
        self.filter_connection = entry.connection_id.clone();
    }
}

pub fn plan_from_run(entry: &HistoryEntry, known_ids: &std::collections::BTreeSet<String>) -> LoadPlan {
    let exists = known_ids.contains(&entry.connection_id);
    LoadPlan {
        sql: entry.sql.clone(),
        connection_id: exists.then(|| entry.connection_id.clone()),
        connection_name: entry.connection_name.clone(),
        database: exists.then(|| entry.database_name.clone()),
        engine: entry.engine.clone(),
        needs_target: !exists,
        execute: false,
        notice_key: (!exists).then_some("history.plan.deletedNotice"),
    }
}

pub fn plan_from_saved(item: &SavedQuery, known_ids: &std::collections::BTreeSet<String>) -> LoadPlan {
    let exists = known_ids.contains(&item.connection_id);
    LoadPlan {
        sql: item.sql.clone(),
        connection_id: exists.then(|| item.connection_id.clone()),
        connection_name: item.connection_name.clone(),
        database: exists.then(|| item.database_name.clone()),
        engine: item.engine.clone(),
        needs_target: !exists,
        execute: false,
        notice_key: (!exists).then_some("history.plan.deletedNotice"),
    }
}

pub async fn load_runs(services: &AppServices) -> Result<Vec<HistoryEntry>, String> {
    services.query.history().await?.list().await
}

pub async fn load_saved(services: &AppServices) -> Result<Vec<SavedQuery>, String> {
    services.query.history().await?.list_saved().await
}

pub async fn save_named(services: &AppServices, request: &SaveRequest) -> Result<bool, String> {
    let written = services.query.history().await?.save_named(request).await?;
    Ok(written.created)
}

pub async fn rename_saved(services: &AppServices, request: &RenameRequest) -> Result<(), String> {
    services.query.history().await?.rename_saved(request).await.map(|_| ())
}

pub async fn delete_saved(services: &AppServices, id: &str) -> Result<(), String> {
    services.query.history().await?.delete_saved(id).await
}

pub async fn delete_run(services: &AppServices, id: &str) -> Result<(), String> {
    services.query.history().await?.delete(id).await
}

pub async fn clear_runs(services: &AppServices) -> Result<(), String> {
    services.query.history().await?.clear().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_history_or_saved_sql_never_sets_execute() {
        let entry = HistoryEntry {
            id: "h1".into(),
            sql: "select 1".into(),
            connection_id: "gone".into(),
            connection_name: "旧库".into(),
            database_name: "app".into(),
            engine: "postgres".into(),
            started_at: "2026-11-07T00:00:00Z".into(),
            finished_at: None,
            status: "ok".into(),
            elapsed_ms: Some(3),
            summary: "1 行".into(),
        };
        let plan = plan_from_run(&entry, &std::collections::BTreeSet::new());
        assert!(!plan.execute);
        assert!(plan.needs_target);
        assert_eq!(plan.notice_key, Some("history.plan.deletedNotice"));
        assert_eq!(plan.sql, "select 1");
        let saved = SavedQuery {
            id: "s1".into(),
            name: "计数".into(),
            sql: "select 2".into(),
            connection_id: "live".into(),
            connection_name: "Atlas".into(),
            database_name: "app".into(),
            engine: "postgres".into(),
            updated_at: "2026-11-07T00:00:00Z".into(),
        };
        let mut known = std::collections::BTreeSet::new();
        known.insert("live".into());
        let plan = plan_from_saved(&saved, &known);
        assert!(!plan.execute);
        assert!(!plan.needs_target);
        assert_eq!(plan.connection_id.as_deref(), Some("live"));
    }
}
