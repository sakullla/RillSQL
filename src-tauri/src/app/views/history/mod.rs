//! 历史和保存的查询只恢复草稿。这里没有任何执行 SQL 的入口。

use crate::application::AppServices;
use crate::query::{HistoryEntry, RenameRequest, SaveRequest, SavedQuery};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySection {
    Runs,
    Saved,
}

/// 列表在界面上的五种可区分状态：读取中、空、筛选无命中、失败、正常。
/// 两个分区（运行记录 / 保存的查询）共用这一套判断，页面不再各自解释“空”。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListPhase {
    Loading,
    Empty,
    NoMatch,
    Failed,
    Rows,
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
    pub preview_open: bool,
    pub rename_open: bool,
    /// 重命名开始时钉住的条目；之后点选别的行不能改变目标。
    pub rename_target: Option<String>,
    pub rename_text: String,
    pub message_key: Option<&'static str>,
    pub failure_key: Option<&'static str>,
    pub failure_detail: String,
    pub confirm_clear: bool,
    pub confirm_delete: bool,
    /// 正在读取本机历史库；读到结果或失败前列表不算“无数据”。
    pub loading: bool,
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
            preview_open: true,
            rename_open: false,
            rename_target: None,
            rename_text: String::new(),
            message_key: None,
            failure_key: None,
            failure_detail: String::new(),
            confirm_clear: false,
            confirm_delete: false,
            loading: false,
            loads: 0,
            executions: 0,
        }
    }
}

/// 运行状态分组：历史里写入的是 running / ok / error 等原始值。
pub fn status_matches(status: &str, filter: &str) -> bool {
    match filter {
        "" | "all" => true,
        "ok" => matches!(status, "ok" | "success" | "done"),
        "failed" => matches!(status, "error" | "failed" | "cancelled"),
        other => status == other,
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
                if !status_matches(&entry.status, &self.filter_status) {
                    return false;
                }
                if needle.is_empty() {
                    return true;
                }
                format!("{} {} {}", entry.sql, entry.connection_name, entry.database_name)
                    .to_ascii_lowercase()
                    .contains(&needle)
            })
            .collect()
    }

    pub fn filtered_saved(&self) -> Vec<&SavedQuery> {
        let needle = self.filter_text.trim().to_ascii_lowercase();
        self.saved
            .iter()
            .filter(|item| {
                if !self.filter_connection.is_empty() && item.connection_id != self.filter_connection {
                    return false;
                }
                needle.is_empty()
                    || item.name.to_ascii_lowercase().contains(&needle)
                    || item.sql.to_ascii_lowercase().contains(&needle)
                    || item.connection_name.to_ascii_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn has_filters(&self) -> bool {
        !self.filter_text.trim().is_empty() || !self.filter_connection.is_empty() || !self.filter_status.is_empty()
    }

    pub fn runs_phase(&self) -> ListPhase {
        self.phase_for(HistorySection::Runs)
    }

    pub fn saved_phase(&self) -> ListPhase {
        self.phase_for(HistorySection::Saved)
    }

    /// 两个分区共用同一条判断顺序：读取中 → 失败 → 无数据 → 筛选无命中 → 有行。
    /// 读取中优先于空态，失败优先于无命中，避免把“还没读到”和“读失败”画成空列表。
    pub fn phase_for(&self, section: HistorySection) -> ListPhase {
        let (total, matched) = match section {
            HistorySection::Runs => (self.runs.len(), self.filtered_runs().len()),
            HistorySection::Saved => (self.saved.len(), self.filtered_saved().len()),
        };
        if self.failure_key.is_some() {
            ListPhase::Failed
        } else if total == 0 && self.loading {
            ListPhase::Loading
        } else if total == 0 {
            ListPhase::Empty
        } else if matched == 0 {
            ListPhase::NoMatch
        } else {
            ListPhase::Rows
        }
    }

    /// 标记一次读取开始；失败与成功都会结束这次读取。
    pub fn begin_load(&mut self) {
        self.loading = true;
    }

    /// 当前分区筛选后可见的行 id，键盘导航和渲染共用同一份顺序。
    pub fn visible_ids(&self) -> Vec<String> {
        if self.section == HistorySection::Saved {
            self.filtered_saved().into_iter().map(|item| item.id.clone()).collect()
        } else {
            self.filtered_runs().into_iter().map(|entry| entry.id.clone()).collect()
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        let selected = if self.section == HistorySection::Saved { self.selected_saved.as_deref() } else { self.selected_run.as_deref() }?;
        self.visible_ids().iter().position(|id| id == selected)
    }

    fn select(&mut self, id: Option<String>) {
        match self.section {
            HistorySection::Runs => self.selected_run = id,
            HistorySection::Saved => self.selected_saved = id,
        }
        // 键盘换行与鼠标点选一致：选中就把预览打开到这一条。
        self.preview_open = true;
    }

    /// 方向键在筛选后的可见行里移动一格；没有选中时从首行开始。返回是否换了行。
    pub fn move_selection(&mut self, step: isize) -> bool {
        let ids = self.visible_ids();
        if ids.is_empty() {
            self.select(None);
            return false;
        }
        let next = match self.selected_index() {
            Some(index) => (index as isize + step).clamp(0, ids.len() as isize - 1) as usize,
            None if step < 0 => ids.len() - 1,
            None => 0,
        };
        let changed = self.selected_index() != Some(next);
        self.select(Some(ids[next].clone()));
        changed
    }

    /// Home / End 跳到筛选后的第一行或最后一行。
    pub fn select_edge(&mut self, first: bool) -> bool {
        let ids = self.visible_ids();
        let Some(id) = (if first { ids.first() } else { ids.last() }).cloned() else {
            self.select(None);
            return false;
        };
        let changed = self.selected_index() != Some(if first { 0 } else { ids.len() - 1 });
        self.select(Some(id));
        changed
    }

    /// 回车载入当前行：只恢复草稿，不执行 SQL。没有选中行时返回 None。
    pub fn enter_command(&self) -> Option<&'static str> {
        let selected = if self.section == HistorySection::Saved {
            self.selected_saved.is_some()
        } else {
            self.selected_run.is_some()
        };
        selected.then_some("history-load")
    }

    /// Delete 删除当前行：运行记录直接进入确认，保存的查询走它自己的确认层。
    pub fn delete_command(&self) -> Option<&'static str> {
        match self.section {
            HistorySection::Runs => self.selected_run.as_ref().map(|_| "history-delete"),
            HistorySection::Saved => self.selected_saved.as_ref().map(|_| "saved-delete"),
        }
    }

    /// 失败、未知或取消的记录把原因放在 summary 里。
    pub fn preview_error(&self) -> Option<&str> {
        if self.section != HistorySection::Runs {
            return None;
        }
        let entry = self.selected_run_entry()?;
        if matches!(entry.status.as_str(), "error" | "failed" | "unknown" | "cancelled") {
            Some(entry.summary.as_str())
        } else {
            None
        }
    }

    pub fn selected_run_entry(&self) -> Option<&HistoryEntry> {
        let id = self.selected_run.as_deref()?;
        self.runs.iter().find(|entry| entry.id == id)
    }

    pub fn selected_saved_item(&self) -> Option<&SavedQuery> {
        let id = self.selected_saved.as_deref()?;
        self.saved.iter().find(|item| item.id == id)
    }

    pub fn plan_run(&self, known_ids: &std::collections::BTreeSet<String>) -> Option<LoadPlan> {
        self.selected_run_entry().map(|entry| plan_from_run(entry, known_ids))
    }

    pub fn plan_saved(&self, known_ids: &std::collections::BTreeSet<String>) -> Option<LoadPlan> {
        self.selected_saved_item().map(|item| plan_from_saved(item, known_ids))
    }

    /// 预览与载入共用同一份计划，保证显示的目标就是会恢复的目标。
    pub fn preview_plan(&self, known_ids: &std::collections::BTreeSet<String>) -> Option<LoadPlan> {
        if self.section == HistorySection::Saved {
            self.plan_saved(known_ids)
        } else {
            self.plan_run(known_ids)
        }
    }

    pub fn filter_selected_connection(&mut self) {
        let selected = if self.section == HistorySection::Saved {
            self.selected_saved_item().map(|item| item.connection_id.clone())
        } else {
            self.selected_run_entry().map(|entry| entry.connection_id.clone())
        };
        if let Some(id) = selected {
            self.filter_connection = id;
        }
    }

    pub fn set_status_filter(&mut self, value: &str) {
        self.filter_status = match value {
            "ok" | "failed" => value.to_owned(),
            _ => String::new(),
        };
    }

    pub fn reset_filters(&mut self) {
        self.filter_text.clear();
        self.filter_connection.clear();
        self.filter_status.clear();
    }

    pub fn begin_rename(&mut self) -> bool {
        let Some((id, name)) = self
            .selected_saved_item()
            .map(|item| (item.id.clone(), item.name.clone()))
        else {
            return false;
        };
        self.rename_text = name;
        self.rename_target = Some(id);
        self.rename_open = true;
        true
    }

    pub fn cancel_rename(&mut self) {
        self.rename_open = false;
        self.rename_target = None;
        self.rename_text.clear();
    }

    /// 空名直接返回 None，调用方给出提示并保留原名称。
    pub fn rename_request(&self) -> Option<RenameRequest> {
        if !self.rename_open {
            return None;
        }
        let id = self.rename_target.clone()?;
        let name = self.rename_text.trim();
        if name.is_empty() {
            return None;
        }
        Some(RenameRequest { id, name: name.to_owned() })
    }

    pub fn mark_failure(&mut self, detail: String) {
        self.loading = false;
        self.failure_key = Some("history.page.failed");
        self.failure_detail = detail;
    }

    pub fn clear_failure(&mut self) {
        self.failure_key = None;
        self.failure_detail.clear();
    }

    pub fn apply_list(&mut self, runs: Vec<HistoryEntry>, saved: Vec<SavedQuery>) {
        self.loading = false;
        self.runs = runs;
        self.saved = saved;
        // 选区回退必须落在当前筛选后仍可见的第一条，不能预览到列表里不存在的记录。
        let first_run = self.filtered_runs().first().map(|entry| entry.id.clone());
        let first_saved = self.filtered_saved().first().map(|item| item.id.clone());
        if self.selected_run.as_ref().is_some_and(|id| !self.runs.iter().any(|entry| &entry.id == id)) {
            self.selected_run = first_run.clone();
        }
        if self.selected_saved.as_ref().is_some_and(|id| !self.saved.iter().any(|item| &item.id == id)) {
            self.selected_saved = first_saved.clone();
        }
        if self.selected_run.is_none() {
            self.selected_run = first_run;
        }
        if self.selected_saved.is_none() {
            self.selected_saved = first_saved;
        }
        if self.rename_target.as_ref().is_some_and(|id| !self.saved.iter().any(|item| &item.id == id)) {
            self.cancel_rename();
        }
        self.clear_failure();
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

    fn run(id: &str, sql: &str, connection: &str, status: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            sql: sql.into(),
            connection_id: connection.into(),
            connection_name: format!("conn-{connection}"),
            database_name: "app".into(),
            engine: "postgres".into(),
            started_at: "2026-11-07T00:00:00Z".into(),
            finished_at: None,
            status: status.into(),
            elapsed_ms: Some(3),
            summary: "1 行".into(),
        }
    }

    fn saved_item(id: &str, name: &str, connection: &str) -> SavedQuery {
        SavedQuery {
            id: id.into(),
            name: name.into(),
            sql: "select 2".into(),
            connection_id: connection.into(),
            connection_name: format!("conn-{connection}"),
            database_name: "app".into(),
            engine: "postgres".into(),
            updated_at: "2026-11-07T00:00:00Z".into(),
        }
    }

    #[test]
    fn loading_history_or_saved_sql_never_sets_execute() {
        let entry = run("h1", "select 1", "gone", "ok");
        let plan = plan_from_run(&entry, &std::collections::BTreeSet::new());
        assert!(!plan.execute);
        assert!(plan.needs_target);
        assert_eq!(plan.notice_key, Some("history.plan.deletedNotice"));
        assert_eq!(plan.sql, "select 1");
        assert_eq!(plan.connection_id, None);
        assert_eq!(plan.database, None);
        let item = saved_item("s1", "计数", "live");
        let mut known = std::collections::BTreeSet::new();
        known.insert("live".into());
        let plan = plan_from_saved(&item, &known);
        assert!(!plan.execute);
        assert!(!plan.needs_target);
        assert_eq!(plan.connection_id.as_deref(), Some("live"));
        assert_eq!(plan.notice_key, None);
    }

    #[test]
    fn search_status_connection_and_phases_are_distinct() {
        let mut page = HistoryPage::default();
        assert_eq!(page.runs_phase(), ListPhase::Empty);
        page.runs = vec![
            run("a", "select 1", "live", "ok"),
            run("b", "select 2", "live", "error"),
            run("c", "delete from t", "other", "ok"),
        ];
        assert_eq!(page.runs_phase(), ListPhase::Rows);
        page.filter_text = "select".into();
        assert_eq!(page.filtered_runs().len(), 2);
        page.filter_text = "missing".into();
        assert_eq!(page.runs_phase(), ListPhase::NoMatch);
        assert!(page.has_filters());
        page.reset_filters();
        page.set_status_filter("failed");
        assert_eq!(page.filtered_runs().len(), 1);
        page.set_status_filter("ok");
        assert_eq!(page.filtered_runs().len(), 2);
        page.set_status_filter("all");
        assert_eq!(page.filtered_runs().len(), 3);
        page.selected_run = Some("c".into());
        page.filter_selected_connection();
        assert_eq!(page.filter_connection, "other");
        assert_eq!(page.filtered_runs().len(), 1);
        assert!(page.has_filters());
        page.mark_failure("磁盘只读".into());
        assert_eq!(page.runs_phase(), ListPhase::Failed);
        page.clear_failure();
        page.reset_filters();
        page.runs.clear();
        assert_eq!(page.runs_phase(), ListPhase::Empty);
    }

    #[test]
    fn saved_search_and_rename_rules_keep_original_values() {
        let mut page = HistoryPage::default();
        assert_eq!(page.saved_phase(), ListPhase::Empty);
        page.saved = vec![saved_item("s1", "计数", "live"), saved_item("s2", "排行榜", "other")];
        assert_eq!(page.saved_phase(), ListPhase::Rows);
        page.filter_text = "排行".into();
        assert_eq!(page.filtered_saved().len(), 1);
        page.filter_text = "没有".into();
        assert_eq!(page.saved_phase(), ListPhase::NoMatch);
        page.reset_filters();

        page.selected_saved = Some("s1".into());
        assert!(page.begin_rename());
        assert_eq!(page.rename_text, "计数");
        // 重命名栏非模态：点选另一条不能改变正在改名的目标。
        page.selected_saved = Some("s2".into());
        assert_eq!(page.rename_request().map(|request| request.id).as_deref(), Some("s1"));
        page.selected_saved = Some("s1".into());
        page.rename_text = "   ".into();
        assert!(page.rename_request().is_none());
        assert_eq!(page.saved[0].name, "计数");
        page.rename_text = "  新名字 ".into();
        let request = page.rename_request().expect("rename");
        assert_eq!(request.id, "s1");
        assert_eq!(request.name, "新名字");
        page.cancel_rename();
        assert!(!page.rename_open);
        assert!(page.rename_request().is_none());
    }

    #[test]
    fn applying_a_list_drops_missing_selection_but_keeps_failure_until_success() {
        let mut page = HistoryPage {
            runs: vec![run("a", "select 1", "live", "ok")],
            saved: vec![saved_item("s1", "计数", "live")],
            selected_run: Some("a".into()),
            selected_saved: Some("s1".into()),
            ..HistoryPage::default()
        };
        page.mark_failure("删不掉".into());
        assert_eq!(page.runs_phase(), ListPhase::Failed);
        page.apply_list(vec![run("b", "select 9", "live", "ok")], Vec::new());
        assert_eq!(page.selected_run.as_deref(), Some("b"));
        assert_eq!(page.selected_saved, None);
        assert!(page.failure_key.is_none());
        assert_eq!(page.runs_phase(), ListPhase::Rows);
        assert!(!page.loading, "读到结果后读取标记必须复位");
    }

    #[test]
    fn loading_failure_empty_and_no_match_stay_distinct_for_both_sections() {
        let mut page = HistoryPage::default();
        assert_eq!(page.runs_phase(), ListPhase::Empty, "没有开始读取的默认页仍是空态");
        page.begin_load();
        assert!(page.loading);
        assert_eq!(page.runs_phase(), ListPhase::Loading, "还没读到不得当成无数据");
        assert_eq!(page.saved_phase(), ListPhase::Loading, "两个分区共用同一判断");
        page.mark_failure("历史库只读".into());
        assert!(!page.loading, "失败结束这次读取");
        assert_eq!(page.runs_phase(), ListPhase::Failed);
        assert_eq!(page.saved_phase(), ListPhase::Failed);
        page.clear_failure();
        page.begin_load();
        page.apply_list(vec![run("a", "select 1", "live", "ok")], vec![saved_item("s1", "计数", "live")]);
        assert_eq!(page.runs_phase(), ListPhase::Rows);
        assert_eq!(page.saved_phase(), ListPhase::Rows);
        page.filter_text = "missing".into();
        assert_eq!(page.runs_phase(), ListPhase::NoMatch);
        assert_eq!(page.saved_phase(), ListPhase::NoMatch);
        // 刷新期间已有行时不换成骨架：继续显示旧行，避免闪空。
        page.begin_load();
        assert_eq!(page.runs_phase(), ListPhase::NoMatch);
        page.apply_list(Vec::new(), Vec::new());
        page.reset_filters();
        assert_eq!(page.runs_phase(), ListPhase::Empty);
    }

    #[test]
    fn keyboard_selection_walks_filtered_rows_and_maps_to_the_same_commands() {
        let mut page = HistoryPage {
            runs: vec![
                run("a", "select 1", "live", "ok"),
                run("b", "select 2", "live", "error"),
                run("c", "delete from t", "other", "ok"),
            ],
            ..HistoryPage::default()
        };
        assert_eq!(page.enter_command(), None, "没有选中行时回车载不了任何东西");
        assert!(page.move_selection(1));
        assert_eq!(page.selected_run.as_deref(), Some("a"));
        assert!(page.preview_open, "键盘换行与点选一样打开预览");
        assert!(page.move_selection(1));
        assert_eq!(page.selected_run.as_deref(), Some("b"));
        assert!(page.move_selection(1));
        assert_eq!(page.selected_run.as_deref(), Some("c"));
        assert!(!page.move_selection(1), "到末行后不再假装移动");
        assert_eq!(page.selected_run.as_deref(), Some("c"));
        assert!(page.select_edge(true));
        assert_eq!(page.selected_run.as_deref(), Some("a"));
        assert!(page.select_edge(false));
        assert_eq!(page.selected_run.as_deref(), Some("c"));
        assert_eq!(page.enter_command(), Some("history-load"));
        assert_eq!(page.delete_command(), Some("history-delete"));

        // 筛选后只在可见行里走：被筛掉的记录不能成为键盘目标。
        page.filter_status = "ok".into();
        assert_eq!(page.visible_ids(), vec!["a".to_owned(), "c".to_owned()]);
        assert!(page.move_selection(-1));
        assert_eq!(page.selected_run.as_deref(), Some("a"));
        assert_eq!(page.delete_command(), Some("history-delete"));

        page.reset_filters();
        page.section = HistorySection::Saved;
        assert!(!page.move_selection(1), "保存分区里没有行时不能凭空选中");
        assert_eq!(page.selected_saved, None);
        page.saved = vec![saved_item("s1", "计数", "live"), saved_item("s2", "排行榜", "other")];
        assert!(page.move_selection(1));
        assert_eq!(page.selected_saved.as_deref(), Some("s1"));
        assert!(page.move_selection(1));
        assert_eq!(page.selected_saved.as_deref(), Some("s2"));
        assert_eq!(page.enter_command(), Some("history-load"));
        assert_eq!(page.delete_command(), Some("saved-delete"));
        page.filter_text = "排行".into();
        assert_eq!(page.visible_ids(), vec!["s2".to_owned()]);
        assert!(!page.select_edge(true), "只有一个命中时 Home 不换行");
        assert_eq!(page.selected_saved.as_deref(), Some("s2"));
        page.filter_text = "没有".into();
        assert!(!page.move_selection(1));
        assert_eq!(page.selected_saved, None, "无命中时键盘不保留已不可见的选中");
    }
}
