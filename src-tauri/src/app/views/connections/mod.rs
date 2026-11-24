//! 七种引擎的连接页状态。保存、测试、打开和导入导出都走应用服务，不走 Tauri command。

mod url;

use std::collections::BTreeMap;
use std::path::Path;

use crate::adapters::{adapter, host_is_uri};
use crate::application::tasks::TaskToken;
use crate::application::{AppServices, TaskBoard, TaskOutcome};
use crate::connections::model::{
    CaMode, ConnectionDraft, Engine, FieldError, Secret, SecretInput, SshAuthKind, TlsMode,
};
use crate::connections::{ConnectionRecord, OpenOutcome, SaveOutcome};
use crate::connections::stages::{StageKind, StageStatus, TestReport, TestStatus};

pub use url::{default_port, parse_connection_url, ParsedUrl};

const OWNER: &str = "connections";

#[derive(Clone)]
#[derive(Default)]
pub struct Secrets {
    pub password: String,
    pub ssh_password: String,
    pub ssh_passphrase: String,
    pub save_password: bool,
}


impl std::fmt::Debug for Secrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secrets([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    Disconnected,
    Connecting,
    Connected,
    Failed,
}

/// 字段级问题：field 是草稿里的字段名（host / port / ssh_host …），key 是通用文案，
/// detail 是具体原文（测试失败消息、服务端字段消息），空表示只有通用文案。
#[derive(Clone, Debug)]
pub struct FieldIssue {
    pub field: String,
    pub key: &'static str,
    pub detail: String,
}

impl FieldIssue {
    pub fn new(field: impl Into<String>, key: &'static str) -> Self {
        Self { field: field.into(), key, detail: String::new() }
    }

    pub fn with_detail(field: impl Into<String>, key: &'static str, detail: impl Into<String>) -> Self {
        Self { field: field.into(), key, detail: detail.into() }
    }
}

#[derive(Clone, Debug)]
#[derive(Default)]
pub struct ConnectionPage {
    pub rows: Vec<ConnectionRecord>,
    pub filter_text: String,
    pub filter_engine: Option<Engine>,
    pub filter_env: String,
    pub selected: Option<String>,
    pub editor: Option<ConnectionDraft>,
    pub secrets: Secrets,
    pub issues: Vec<FieldIssue>,
    pub status: BTreeMap<String, LinkStatus>,
    pub message_key: Option<&'static str>,
    pub message_detail: String,
    pub confirm_delete: bool,
    pub executed_sql: bool,
    /// 最近一次发起测试的代号；只有这一代（或更新的）结果能动"正在测试"指示。
    pub test_generation: u64,
    /// 正在跑的测试代号；None 表示没有在跑的测试。过期结果不能把它清掉。
    pub test_running_generation: Option<u64>,
    /// 持久化的最近使用顺序（Store 的 connection_usage）。启动台的"最近连接"只认它。
    pub recent_ids: Vec<String>,
    /// 连接清单至少成功读到过一次。之前不能断言“某条连接已不存在”。
    pub loaded: bool,
    /// 每条连接最近一次连接失败的原因。状态栏的提示会被下一条消息覆盖，
    /// 这里按连接留住，资源管理器在失败的连接下面直接写出原因。连上后清掉。
    pub failures: BTreeMap<String, String>,
    /// 最近一次打开失败是因为缺密码（false）或密码被拒绝（true）：外壳据此弹出密码输入框，
    /// 取走后清空。
    pub credential_prompt: Option<(String, bool)>,
    /// 最近一次测试的完整结果（各阶段成败）。只属于屏幕上这份草稿：草稿一改就清掉，
    /// 表单不会在改了地址之后还挂着上一份配置的“连接成功”。
    pub test_report: Option<TestReport>,
    /// 同一轮弹窗内跨文件型（SQLite）与联网引擎来回切换时暂存上次填好的字段，误点 SQLite 再切回不会冲掉主机/用户/TLS。
    pub editor_network_stash: Option<ConnectionDraft>,
    pub editor_file_stash: Option<String>,
}

/// 打开失败是不是缺密码或密码被拒：返回 Some(被拒绝)。服务端拒绝凭据的错误码随驱动不同
/// （PostgreSQL 落在通用的阶段失败码上），所以以失败阶段为准：数据库认证阶段失败即凭据问题。
pub fn credential_failure(report: &TestReport) -> Option<bool> {
    use crate::connections::error_codes;
    let failed = report.stages.iter().find(|stage| stage.status == StageStatus::Err)?;
    if failed.kind != StageKind::DatabaseAuthentication {
        return None;
    }
    let code = failed.code.as_deref().or(report.code.as_deref());
    Some(code != Some(error_codes::CONN_PASSWORD_REQUIRED))
}


impl ConnectionPage {
    pub fn begin(&mut self, tasks: &TaskBoard) -> TaskToken {
        tasks.supersede(OWNER);
        tasks.begin(OWNER)
    }

    pub fn accept_list(&mut self, tasks: &TaskBoard, token: TaskToken, rows: Vec<ConnectionRecord>) -> bool {
        match tasks.accept(OWNER, token, rows) {
            TaskOutcome::Current { value, .. } => {
                self.rows = value;
                self.loaded = true;
                true
            }
            TaskOutcome::Stale { .. } | TaskOutcome::Cancelled { .. } => false,
        }
    }

    pub fn filtered(&self) -> Vec<&ConnectionRecord> {
        let needle = self.filter_text.trim().to_ascii_lowercase();
        self.rows
            .iter()
            .filter(|row| {
                if self.filter_engine.is_some_and(|engine| row.saved.engine != engine) {
                    return false;
                }
                if !self.filter_env.is_empty() && crate::connections::model::normalize_env_id(&row.saved.env) != self.filter_env {
                    return false;
                }
                if needle.is_empty() {
                    return true;
                }
                let saved = &row.saved;
                let haystack = format!(
                    "{} {} {} {} {} {} {}",
                    saved.name,
                    saved.host.as_deref().unwrap_or(""),
                    saved.username.as_deref().unwrap_or(""),
                    saved.file_path.as_deref().unwrap_or(""),
                    saved.database.as_deref().unwrap_or(""),
                    saved.engine.as_str(),
                    saved.engine.label_zh()
                )
                .to_ascii_lowercase();
                haystack.contains(&needle)
            })
            .collect()
    }

    pub fn start_new(&mut self, engine: Engine) {
        self.editor = Some(blank_draft(engine));
        self.editor_network_stash = None;
        self.editor_file_stash = None;
        self.secrets = Secrets::default();
        self.issues.clear();
        self.confirm_delete = false;
        self.message_key = None;
        self.message_detail.clear();
        self.test_report = None;
    }

    /// 草稿或本次机密被改动：上一次测试/保存的结论说的已经不是屏幕上这份配置，收起来。
    /// 正在跑的测试不动——它的结果回来时会因为指纹对不上而被丢弃。
    pub fn draft_touched(&mut self) {
        self.test_report = None;
        if self.message_key != Some("connections.editor.test.running") {
            self.message_key = None;
            self.message_detail.clear();
        }
    }

    /// 在表单里换数据库类型：能沿用的字段都留着（见 [`switch_engine`]），跨 SQLite 来回切也恢复先前暂存的字段。
    pub fn change_engine(&mut self, engine: Engine) {
        let Some(draft) = self.editor.clone() else { return };
        if draft.engine == engine {
            return;
        }
        let was_file = file_based(draft.engine);
        let to_file = file_based(engine);
        if was_file {
            if !draft.file_path.trim().is_empty() {
                self.editor_file_stash = Some(draft.file_path.clone());
            }
        } else {
            self.editor_network_stash = Some(draft.clone());
        }
        let mut next = if was_file && !to_file {
            if let Some(mut stashed) = self.editor_network_stash.clone() {
                stashed.id = draft.id.clone();
                stashed.name = draft.name.clone();
                let file_blank = blank_draft(draft.engine);
                if draft.env != file_blank.env {
                    stashed.env = draft.env.clone();
                }
                switch_engine(&stashed, engine)
            } else {
                switch_engine(&draft, engine)
            }
        } else {
            switch_engine(&draft, engine)
        };
        if to_file && next.file_path.is_empty() {
            if let Some(file_path) = &self.editor_file_stash {
                next.file_path = file_path.clone();
            }
        }
        self.editor = Some(next);
        self.issues.clear();
        self.draft_touched();
    }

    pub fn edit_selected(&mut self) {
        let Some(id) = self.selected.clone() else { return };
        let Some(row) = self.rows.iter().find(|row| row.saved.id == id) else { return };
        self.editor = Some(draft_from_saved(&row.saved));
        self.editor_network_stash = None;
        self.editor_file_stash = None;
        self.secrets = Secrets::default();
        self.issues.clear();
        self.confirm_delete = false;
        self.test_report = None;
    }

    pub fn apply_url(&mut self, text: &str) -> Result<(), &'static str> {
        let parsed = parse_connection_url(text)?;
        let mut draft = self.editor.clone().unwrap_or_else(|| blank_draft(parsed.engine));
        draft.engine = parsed.engine;
        draft.host = parsed.host;
        draft.port = parsed.port;
        draft.database = parsed.database;
        draft.username = parsed.username;
        if let Some(path) = parsed.file_path {
            draft.file_path = path;
        }
        if let Some(mode) = parsed.tls_mode {
            draft.tls_mode = mode;
        }
        if adapter(parsed.engine).meta().defaults_to_local() && draft.env == "dev" {
                draft.env = "local".into();
            }
        self.secrets.password = parsed.password;
        self.editor = Some(draft);
        self.issues = self.validate();
        Ok(())
    }

    pub fn validate(&self) -> Vec<FieldIssue> {
        let Some(draft) = &self.editor else { return Vec::new() };
        validate_draft(draft)
    }

    pub fn duplicate_selected(&mut self) {
        let Some(id) = self.selected.clone() else { return };
        let Some(row) = self.rows.iter().find(|row| row.saved.id == id) else { return };
        let mut draft = draft_from_saved(&row.saved);
        draft.id = None;
        draft.name = format!("{} copy", draft.name.trim());
        draft.save_password = false;
        self.editor = Some(draft);
        self.editor_network_stash = None;
        self.editor_file_stash = None;
        self.secrets = Secrets::default();
        self.issues.clear();
        self.test_report = None;
    }

    /// 只打开确认，不因为再次调用就放行删除。
    pub fn arm_delete(&mut self) -> bool {
        if self.selected.is_none() {
            return false;
        }
        self.confirm_delete = true;
        self.message_key = Some("connections.deleteDialog.title");
        true
    }

    pub fn mark_disconnected(&mut self, id: &str) {
        self.status.insert(id.to_owned(), LinkStatus::Disconnected);
        self.message_key = Some("connections.page.menuDisconnect");
    }

    pub fn accept_notice(&mut self, tasks: &TaskBoard, token: TaskToken, detail: String) -> bool {
        match tasks.accept(OWNER, token, detail) {
            TaskOutcome::Current { value, .. } => {
                self.message_detail = value;
                self.message_key = Some("connections.status.error");
                true
            }
            TaskOutcome::Stale { .. } | TaskOutcome::Cancelled { .. } => false,
        }
    }

    pub fn clear_list_filters(&mut self) {
        self.filter_text.clear();
        self.filter_engine = None;
        self.filter_env.clear();
    }

    pub fn select_engine(&mut self, engine: Engine) {
        self.start_new(engine);
    }

    /// SSH 认证方式在原位来回切；界面上的两个分段控件也落到这里。
    pub fn cycle_ssh_auth(&mut self) {
        let Some(draft) = self.editor.as_mut() else { return };
        draft.ssh_auth = match draft.ssh_auth {
            SshAuthKind::Key => SshAuthKind::Password,
            SshAuthKind::Password => SshAuthKind::Key,
        };
    }

    pub fn cycle_ca_mode(&mut self) {
        let Some(draft) = self.editor.as_mut() else { return };
        draft.ca_mode = match draft.ca_mode {
            CaMode::System => CaMode::File,
            CaMode::File => CaMode::System,
        };
    }

    pub fn toggle_ssh(&mut self) {
        if let Some(draft) = self.editor.as_mut() {
            draft.ssh_on = !draft.ssh_on;
        }
    }

    /// 清掉某个字段的错误。用户开始改这个字段，就不该继续显示上一次的结论。
    pub fn clear_issue(&mut self, field: &str) {
        self.issues.retain(|issue| issue.field != field);
    }

    /// 启动台“最近连接”：recent_ids 已经按持久化的最近成功打开时间排好序，这里只按当前列表取行。
    /// 不认识的 id 直接跳过，绝不回退成“列表前几条”冒充最近。
    pub fn recent<'a>(&'a self, recent_ids: &[String], limit: usize) -> Vec<&'a ConnectionRecord> {
        let mut rows = Vec::new();
        for id in recent_ids {
            if rows.len() >= limit {
                break;
            }
            if let Some(row) = self.rows.iter().find(|row| &row.saved.id == id) {
                rows.push(row);
            }
        }
        rows
    }

    /// 发起一次测试：记下这一代的代号，并让"正在测试"只属于它。
    pub fn expect_test(&mut self, generation: u64) {
        self.test_generation = self.test_generation.max(generation);
        self.test_running_generation = Some(generation);
        self.message_key = Some("connections.editor.test.running");
        self.message_detail.clear();
        self.test_report = None;
    }

    /// 还有一次测试在跑（页脚指示点与禁用重复提交都读它）。
    pub fn test_running(&self) -> bool {
        self.test_running_generation.is_some()
    }

    /// 启动台“最近连接”：顺序只来自持久化用量，不认识的 id 直接跳过。
    pub fn launcher_recent(&self, limit: usize) -> Vec<&ConnectionRecord> {
        self.recent(&self.recent_ids, limit)
    }

    /// 启动台“全部连接”：受搜索与筛选影响，但不受最近使用影响。
    pub fn launcher_all(&self) -> Vec<&ConnectionRecord> {
        self.filtered()
    }

    /// 列表行的状态；不认识的行按未连接。
    pub fn row_status(&self, id: &str) -> LinkStatus {
        self.status.get(id).copied().unwrap_or(LinkStatus::Disconnected)
    }

    /// 测试失败落到具体字段。只给一条：第一个失败/等待阶段决定用户下一步该改哪个字段。
    fn failure_issues(report: &TestReport, draft: Option<&ConnectionDraft>) -> Vec<FieldIssue> {
        let Some(draft) = draft else { return Vec::new() };
        field_issue_for_failure(report, draft).into_iter().collect()
    }

    /// 记录一次测试结果。fingerprint 是发起请求时的草稿指纹，generation 是发起时的代号：
    /// 屏幕上的草稿、本次机密或“表单是否还开着”已经变了就丢弃这次结果；代号已经被更新的一次
    /// 测试取代时，连“正在测试”也不许碰——否则新那一次的指示会被旧结果收走（P3）。
    pub fn note_test(
        &mut self,
        tasks: &TaskBoard,
        token: TaskToken,
        generation: u64,
        fingerprint: u64,
        report: Result<TestReport, String>,
    ) -> bool {
        if generation < self.test_generation {
            // 更新的一次测试已经发出：旧结果的任何写入（含指示）都不属于当前屏幕。
            return false;
        }
        let running_is_mine = self.test_running_generation == Some(generation);
        if draft_fingerprint(&self.editor, &self.secrets) != fingerprint {
            // 过期结果：只收起自己那一代的“正在测试”，不写状态、消息或字段错误。
            if running_is_mine {
                self.test_running_generation = None;
            }
            if running_is_mine && self.message_key == Some("connections.editor.test.running") {
                self.message_key = None;
                self.message_detail.clear();
            }
            return false;
        }
        let report = match tasks.accept(OWNER, token, report) {
            TaskOutcome::Current { value, .. } => value,
            TaskOutcome::Stale { .. } | TaskOutcome::Cancelled { .. } => {
                if running_is_mine {
                    self.test_running_generation = None;
                }
                return false;
            }
        };
        if running_is_mine {
            self.test_running_generation = None;
        }
        // 状态按屏幕上的草稿归属，不按列表选中行：新建草稿的测试不该标记到别的连接上。
        let row_id = self.editor.as_ref().and_then(|draft| draft.id.clone());
        match report {
            Ok(report) => {
                self.message_detail = report.summary.clone().unwrap_or_default();
                self.message_key = Some(match report.status {
                    TestStatus::Ok => "connections.editor.test.ok",
                    TestStatus::Wait => "connections.editor.test.wait",
                    TestStatus::Err => "connections.editor.test.failed",
                });
                self.test_report = Some(report.clone());
                match report.status {
                    // 测试成功只说明这份配置能连上，不等于已经建立会话：不写 Connected。
                    TestStatus::Ok | TestStatus::Wait => self.issues.clear(),
                    TestStatus::Err => {
                        self.issues = Self::failure_issues(&report, self.editor.as_ref());
                        if let Some(id) = row_id {
                            self.status.insert(id.clone(), LinkStatus::Failed);
                            self.failures.insert(id, failure_reason(&report));
                        }
                    }
                }
            }
            Err(detail) => {
                self.message_detail = detail;
                self.message_key = Some("connections.status.error");
                self.issues.clear();
                self.test_report = None;
                if let Some(id) = row_id {
                    self.status.insert(id.clone(), LinkStatus::Failed);
                    self.failures.insert(id, self.message_detail.clone());
                }
            }
        }
        self.executed_sql = false;
        true
    }

    pub fn note_open(&mut self, tasks: &TaskBoard, token: TaskToken, outcome: Result<OpenOutcome, String>) -> bool {
        let outcome = match tasks.accept(OWNER, token, outcome) {
            TaskOutcome::Current { value, .. } => value,
            TaskOutcome::Stale { .. } | TaskOutcome::Cancelled { .. } => return false,
        };
        let Some(id) = self.selected.clone() else { return true };
        match outcome {
            Ok(outcome) if outcome.connected => {
                // 成功提示说连上了哪条连接；最后一个检查阶段的内部说明（如 foreign_keys ON）不放进状态栏。
                let name = self.rows.iter().find(|row| row.saved.id == id).map(|row| row.saved.name.clone()).unwrap_or_default();
                self.failures.remove(&id);
                self.status.insert(id, LinkStatus::Connected);
                self.message_key = Some("connections.status.connected");
                self.message_detail = if name.is_empty() { String::new() } else { format!("· {name}") };
            }
            Ok(outcome) => {
                self.credential_prompt = credential_failure(&outcome.report).map(|rejected| (id.clone(), rejected));
                self.failures.insert(id.clone(), failure_reason(&outcome.report));
                self.status.insert(id, LinkStatus::Failed);
                self.message_key = Some("connections.status.error");
                self.message_detail = outcome.report.summary.or(outcome.report.hint).unwrap_or_default();
            }
            Err(detail) => {
                self.failures.insert(id.clone(), detail.clone());
                self.status.insert(id, LinkStatus::Failed);
                self.message_key = Some("connections.status.error");
                self.message_detail = detail;
            }
        }
        self.executed_sql = false;
        true
    }
}

/// 资源管理器里留给一条连接的失败原因：首行是失败阶段的原文，有处理建议时另起一行附上。
fn failure_reason(report: &TestReport) -> String {
    let message = crate::connections::report_failure_text(report);
    match report.hint.as_deref().map(str::trim).filter(|hint| !hint.is_empty() && *hint != message.trim()) {
        Some(hint) => format!("{message}
{hint}"),
        None => message,
    }
}

/// 新建草稿。默认端口、默认库、是否按本地开发处理都来自引擎适配器的 EngineMeta。
pub fn blank_draft(engine: Engine) -> ConnectionDraft {
    let meta = adapter(engine).meta();
    let local = meta.defaults_to_local();
    ConnectionDraft {
        id: None,
        name: String::new(),
        engine,
        env: if local { "local".into() } else { "dev".into() },
        host: String::new(),
        port: default_port(engine),
        database: meta.default_database().into(),
        username: String::new(),
        file_path: String::new(),
        replica_set: String::new(),
        tls_mode: if local { TlsMode::Off } else { TlsMode::Verify },
        ca_mode: CaMode::System,
        ca_file: String::new(),
        client_cert: String::new(),
        client_key: String::new(),
        ssh_on: false,
        ssh_host: String::new(),
        ssh_port: "22".into(),
        ssh_user: String::new(),
        ssh_auth: SshAuthKind::Key,
        ssh_key: String::new(),
        save_password: !meta.file_based(),
    }
}

/// 文件型引擎（本地文件即数据库，没有主机/端口/TLS/SSH）。
pub fn file_based(engine: Engine) -> bool {
    adapter(engine).meta().file_based()
}

/// 表单里换数据库类型时的新草稿。
///
/// 以前换类型等于清空表单：填好主机、用户、跳板机之后发现该选 MariaDB 而不是 MySQL，就得全部重填。
/// 现在两种联网的引擎之间沿用主机、用户名、默认库、TLS 与 SSH；端口、环境、TLS 模式只在
/// 用户改过（不等于旧引擎的默认值）时才沿用，否则换成新引擎的默认值。
/// 文件型（SQLite）与联网引擎之间没有可沿用的地址，只留名称与环境。
pub fn switch_engine(current: &ConnectionDraft, engine: Engine) -> ConnectionDraft {
    if current.engine == engine {
        return current.clone();
    }
    let before = blank_draft(current.engine);
    let mut next = blank_draft(engine);
    next.id = current.id.clone();
    next.name = current.name.clone();
    if current.env != before.env {
        next.env = current.env.clone();
    }
    if file_based(current.engine) || file_based(engine) {
        return next;
    }
    // 整串连接串（如 mongodb://）只有认它的引擎才当主机；换到别的引擎时它不是一个主机名。
    let uri_host = crate::adapters::registry::ENGINES.into_iter().any(|other| host_is_uri(other, &current.host));
    if !uri_host || host_is_uri(engine, &current.host) {
        next.host = current.host.clone();
    }
    let port = current.port.trim();
    if !port.is_empty() && port != before.port.trim() {
        next.port = current.port.clone();
    }
    next.username = current.username.clone();
    // 库索引（Redis 的“库”从 0 起）不是库名：两边不互相沿用。
    if !adapter(current.engine).meta().database_is_index() && !adapter(engine).meta().database_is_index() {
        next.database = current.database.clone();
    }
    if current.tls_mode != before.tls_mode {
        next.tls_mode = current.tls_mode;
    }
    next.ca_mode = current.ca_mode;
    next.ca_file = current.ca_file.clone();
    next.client_cert = current.client_cert.clone();
    next.client_key = current.client_key.clone();
    next.ssh_on = current.ssh_on;
    next.ssh_host = current.ssh_host.clone();
    next.ssh_port = current.ssh_port.clone();
    next.ssh_user = current.ssh_user.clone();
    next.ssh_auth = current.ssh_auth;
    next.ssh_key = current.ssh_key.clone();
    next.save_password = current.save_password;
    next
}

/// 新建草稿是不是还没填过东西：只选了类型或环境不算。关闭表单时据此决定要不要先问一句。
pub fn draft_is_untouched(draft: &ConnectionDraft, secrets: &Secrets) -> bool {
    let mut blank = blank_draft(draft.engine);
    blank.env = draft.env.clone();
    draft.id.is_none() && *draft == blank && secrets.password.is_empty() && secrets.ssh_password.is_empty() && secrets.ssh_passphrase.is_empty()
}

pub fn draft_from_saved(saved: &crate::connections::model::SavedConnection) -> ConnectionDraft {
    let tls = saved.tls.clone();
    let ssh = saved.ssh.clone();
    ConnectionDraft {
        id: Some(saved.id.clone()),
        name: saved.name.clone(),
        engine: saved.engine,
        env: saved.env.clone(),
        host: saved.host.clone().unwrap_or_default(),
        port: saved.port.map(|port| port.to_string()).unwrap_or_default(),
        database: saved.database.clone().unwrap_or_default(),
        username: saved.username.clone().unwrap_or_default(),
        file_path: saved.file_path.clone().unwrap_or_default(),
        replica_set: saved.replica_set.clone().unwrap_or_default(),
        tls_mode: tls.as_ref().map(|item| item.mode).unwrap_or(TlsMode::Off),
        ca_mode: tls.as_ref().map(|item| item.ca_mode).unwrap_or(CaMode::System),
        ca_file: tls.and_then(|item| item.ca_file).unwrap_or_default(),
        client_cert: saved.tls.as_ref().and_then(|item| item.client_cert.clone()).unwrap_or_default(),
        client_key: saved.tls.as_ref().and_then(|item| item.client_key.clone()).unwrap_or_default(),
        ssh_on: ssh.is_some(),
        ssh_host: ssh.as_ref().map(|item| item.host.clone()).unwrap_or_default(),
        ssh_port: ssh.as_ref().map(|item| item.port.to_string()).unwrap_or_else(|| "22".into()),
        ssh_user: ssh.as_ref().map(|item| item.username.clone()).unwrap_or_default(),
        ssh_auth: ssh.as_ref().map(|item| item.auth).unwrap_or(SshAuthKind::Key),
        ssh_key: ssh.and_then(|item| item.key_path).unwrap_or_default(),
        save_password: saved.password_saved,
    }
}

pub fn validate_draft(draft: &ConnectionDraft) -> Vec<FieldIssue> {
    let mut issues = Vec::new();
    let push = |issues: &mut Vec<FieldIssue>, field: &str, key: &'static str| {
        issues.push(FieldIssue::new(field, key));
    };
    let meta = adapter(draft.engine).meta();
    if meta.file_based() {
        if draft.file_path.trim().is_empty() {
            push(&mut issues, "file_path", "connections.validate.chooseFile");
        }
        return issues;
    }
    if draft.host.trim().is_empty() {
        push(&mut issues, "host", "connections.validate.hostRequired");
    }
    let uri_host = meta.host_is_uri(&draft.host);
    if !uri_host {
        let port = draft.port.trim().parse::<u16>().ok();
        if port.is_none_or(|port| port == 0) {
            push(&mut issues, "port", "connections.validate.portRange");
        }
    }
    if draft.username.trim().is_empty() && !meta.username_optional() {
        push(&mut issues, "username", "connections.validate.userRequired");
    }
    if meta.database_is_index() && !draft.database.trim().is_empty() && !draft.database.trim().chars().all(|ch| ch.is_ascii_digit()) {
        push(&mut issues, "database", "connections.validate.redisDbIndex");
    }
    if uri_host && draft.ssh_on {
        push(&mut issues, "ssh_host", "connections.validate.mongoUriSsh");
    }
    if draft.ssh_on && draft.ssh_host.trim().is_empty() {
        push(&mut issues, "ssh_host", "connections.validate.sshHostRequired");
    }
    if draft.ssh_on && draft.ssh_user.trim().is_empty() {
        push(&mut issues, "ssh_user", "connections.validate.sshUserRequired");
    }
    if draft.ssh_on && draft.ssh_port.trim().parse::<u16>().ok().is_none_or(|port| port == 0) {
        push(&mut issues, "ssh_port", "connections.validate.portRange");
    }
    if draft.tls_mode != TlsMode::Off && draft.ca_mode == CaMode::File && draft.ca_file.trim().is_empty() {
        push(&mut issues, "ca_file", "connections.validate.caFileRequired");
    }
    issues
}

pub fn transport_key(draft: &ConnectionDraft) -> &'static str {
    if file_based(draft.engine) {
        return "connections.page.localFile";
    }
    match draft.tls_mode {
        TlsMode::Verify => "connections.page.tlsVerify",
        TlsMode::Require => "connections.page.tlsRequire",
        TlsMode::Off => "connections.page.tlsOff",
    }
}

/// 一次列表刷新同时带回持久化的最近使用顺序：启动台的"最近连接"不必再猜。
#[derive(Clone, Debug, Default)]
pub struct ConnectionSnapshot {
    pub rows: Vec<ConnectionRecord>,
    pub recent: Vec<String>,
}

pub async fn list_rows(services: &AppServices) -> Result<ConnectionSnapshot, String> {
    let rows = services.list_connections().await?;
    // 用量表读失败不该挡住连接列表：最近一栏退化为空，而不是整页报错。
    let recent = services.connections.recent_ids(12).await.unwrap_or_default();
    Ok(ConnectionSnapshot { rows, recent })
}

pub async fn save_draft(services: &AppServices, draft: ConnectionDraft, secrets: &Secrets) -> Result<SaveOutcome, String> {
    let password = none_if_empty(&secrets.password);
    let ssh_password = none_if_empty(&secrets.ssh_password);
    let ssh_passphrase = none_if_empty(&secrets.ssh_passphrase);
    services
        .save_connection(draft, password, ssh_password, ssh_passphrase)
        .await
        .map_err(|err| err.message())
}

pub async fn delete_row(services: &AppServices, id: &str) -> Result<(), String> {
    services.delete_connection(id).await.map(|_| ())
}

/// 测试当前屏幕上的草稿：它就是列表里那一行（或还没保存的新行）的编辑结果。
/// 新建未保存的草稿也能测；编辑已有连接时测的是草稿，而不是该 id 在配置库里的旧值。
/// 只读配置库/凭据库，不写连接配置，也不更新最近使用时间。
pub async fn test_row(services: &AppServices, draft: ConnectionDraft, secrets: &Secrets) -> Result<TestReport, String> {
    services.connections.test_candidate(draft, secret_input(secrets)).await
}

/// 草稿 + 本次机密的内容指纹：判断一个异步测试结果是否还对应当前屏幕上的编辑。
/// 只看内容不看时间；表单关闭、换连接或改任意字段都会得到不同的值。
pub fn draft_fingerprint(editor: &Option<ConnectionDraft>, secrets: &Secrets) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match editor {
        Some(draft) => {
            1u8.hash(&mut hasher);
            // 草稿是普通配置（不含密码），可以直接序列化后参与哈希。
            serde_json::to_string(draft).unwrap_or_default().hash(&mut hasher);
        }
        None => 0u8.hash(&mut hasher),
    }
    secrets.password.hash(&mut hasher);
    secrets.ssh_password.hash(&mut hasher);
    secrets.ssh_passphrase.hash(&mut hasher);
    hasher.finish()
}

/// 失败的第一落点：用户最该先改的那个字段。detail 保留服务端原文，字段旁只显示通用文案。
pub fn field_issue_for_failure(report: &TestReport, draft: &ConnectionDraft) -> Option<FieldIssue> {
    use crate::connections::error_codes;
    let stage = report
        .stages
        .iter()
        .find(|stage| matches!(stage.status, StageStatus::Err | StageStatus::Wait))?;
    let field = match stage.kind {
        StageKind::OpenFile | StageKind::ReadSchema | StageKind::ForeignKeys => "file_path",
        StageKind::SshHostIdentity | StageKind::Tunnel => "ssh_host",
        StageKind::SshAuthentication => {
            if draft.ssh_auth == SshAuthKind::Password {
                "ssh_password"
            } else {
                "ssh_host"
            }
        }
        StageKind::Network | StageKind::ServerVersion => "host",
        StageKind::Tls => {
            if draft.ca_mode == CaMode::File {
                "ca_file"
            } else {
                "host"
            }
        }
        // 凭据被服务端拒绝时分不清是用户名还是密码：落在凭据组上，而不是一律怪用户名。
        StageKind::DatabaseAuthentication => match report.code.as_deref() {
            Some(error_codes::CONN_PASSWORD_REQUIRED) => "password",
            Some(error_codes::CONN_AUTH_FAILED) | Some(error_codes::CONN_REDIS_AUTH_FAILED) => "credentials",
            _ => "username",
        },
    };
    Some(FieldIssue::with_detail(field, "connections.status.error", stage.message.clone()))
}

fn secret_input(secrets: &Secrets) -> SecretInput {
    SecretInput {
        password: none_if_empty(&secrets.password).map(Secret::new),
        ssh_password: none_if_empty(&secrets.ssh_password).map(Secret::new),
        ssh_passphrase: none_if_empty(&secrets.ssh_passphrase).map(Secret::new),
        save_password: false,
    }
}

pub async fn open_row(services: &AppServices, id: &str, secrets: &Secrets) -> Result<OpenOutcome, String> {
    services
        .open_connection_with_save(
            id,
            none_if_empty(&secrets.password),
            none_if_empty(&secrets.ssh_password),
            none_if_empty(&secrets.ssh_passphrase),
            secrets.save_password,
        )
        .await
}

pub fn disconnect_row(services: &AppServices, id: &str) {
    services.disconnect(id);
}

pub async fn export_rows(services: &AppServices, ids: Option<Vec<String>>, passphrase: Option<&str>, path: &Path) -> Result<usize, String> {
    services.export_connections(ids, passphrase, path).await
}

pub async fn import_rows(services: &AppServices, path: &Path, passphrase: Option<&str>) -> Result<(usize, Vec<String>), String> {
    let parsed = services.import_connections(path, passphrase).await?;
    let mut imported = 0;
    let mut skipped = parsed.skipped.into_iter().map(|row| format!("{}: {}", row.name, row.reason)).collect::<Vec<_>>();
    for saved in parsed.connections {
        let draft = draft_from_saved(&saved);
        match save_draft(services, draft, &Secrets::default()).await {
            Ok(_) => imported += 1,
            Err(err) => skipped.push(format!("{}: {err}", saved.name)),
        }
    }
    Ok((imported, skipped))
}

fn none_if_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
}

pub fn engines() -> [Engine; 7] {
    crate::adapters::registry::CONNECTION_FORM_ORDER
}

/// 给领域校验用的最小合法草稿。TLS/SSH 材料留在草稿里，密码不进这个结构。
#[cfg(test)]
pub fn sample_draft(engine: Engine) -> ConnectionDraft {
    let mut draft = blank_draft(engine);
    draft.name = format!("sample-{}", engine.as_str());
    match engine {
        Engine::Sqlite => draft.file_path = "notes.db".into(),
        Engine::Redis => {
            draft.host = "127.0.0.1".into();
            draft.database = "0".into();
        }
        Engine::Mongodb => {
            draft.host = "127.0.0.1".into();
            draft.database = "admin".into();
        }
        _ => {
            draft.host = "db.example.test".into();
            draft.username = "app".into();
            draft.database = "app".into();
        }
    }
    draft
}

#[cfg(test)]
pub fn with_tls_ssh(engine: Engine) -> ConnectionDraft {
    let mut draft = sample_draft(engine);
    if engine == Engine::Sqlite {
        return draft;
    }
    draft.tls_mode = TlsMode::Verify;
    draft.ca_mode = CaMode::File;
    draft.ca_file = "ca.pem".into();
    draft.client_cert = "client.pem".into();
    draft.client_key = "client.key".into();
    draft.ssh_on = true;
    draft.ssh_host = "bastion.example.test".into();
    draft.ssh_port = "22".into();
    draft.ssh_user = "jump".into();
    draft.ssh_auth = SshAuthKind::Key;
    draft.ssh_key = "id_ed25519".into();
    let _ = crate::connections::model::TlsSettings {
        mode: draft.tls_mode,
        server_name: draft.host.clone(),
        ca_mode: draft.ca_mode,
        ca_file: Some(draft.ca_file.clone()),
        client_cert: Some(draft.client_cert.clone()),
        client_key: Some(draft.client_key.clone()),
    };
    let _ = crate::connections::model::SshSettings {
        host: draft.ssh_host.clone(),
        port: 22,
        username: draft.ssh_user.clone(),
        auth: draft.ssh_auth,
        key_path: Some(draft.ssh_key.clone()),
    };
    let _ = Secret::new("not-stored-on-draft");
    draft
}

#[allow(dead_code)]
fn _field_error_is_domain(error: FieldError) -> String {
    error.message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::TaskBoard;
    use crate::connections::model::normalize;

    #[test]
    fn validation_and_url_fill_cover_each_engine() {
        for engine in engines() {
            let draft = sample_draft(engine);
            assert!(validate_draft(&draft).is_empty(), "{engine:?}");
            assert!(normalize(&draft, "id".into()).is_ok(), "{engine:?}");
        }
        let mut page = ConnectionPage::default();
        page.start_new(Engine::Postgres);
        page.apply_url("postgres://app:s3cret@db.example.test:5432/app?sslmode=verify-full").unwrap();
        let draft = page.editor.clone().unwrap();
        assert_eq!(draft.engine, Engine::Postgres);
        assert_eq!(draft.tls_mode, TlsMode::Verify);
        assert_eq!(page.secrets.password, "s3cret");
        assert!(!format!("{draft:?}").contains("s3cret"));
        page.apply_url("rediss://:secret@cache.example.test/0").unwrap();
        assert_eq!(page.editor.as_ref().unwrap().engine, Engine::Redis);
        assert_eq!(page.editor.as_ref().unwrap().tls_mode, TlsMode::Require);
        page.apply_url("mongodb+srv://cluster.example.test/shop").unwrap();
        assert!(!page.editor.as_ref().unwrap().ssh_on);
        assert_eq!(page.editor.as_ref().unwrap().tls_mode, TlsMode::Require);
        let mut mongo = page.editor.clone().unwrap();
        mongo.ssh_on = true;
        assert!(validate_draft(&mongo).iter().any(|issue| issue.key == "connections.validate.mongoUriSsh"));
        let mut remote = sample_draft(Engine::Mysql);
        remote.tls_mode = TlsMode::Verify;
        remote.ca_mode = CaMode::File;
        assert!(validate_draft(&remote).iter().any(|issue| issue.key == "connections.validate.caFileRequired"));
        let secured = with_tls_ssh(Engine::Postgres);
        assert!(validate_draft(&secured).is_empty());
        let saved = normalize(&secured, "pg".into()).unwrap();
        assert!(saved.tls.is_some());
        assert!(saved.ssh.is_some());
        assert!(!serde_json::to_string(&saved).unwrap().contains("s3cret"));
    }

    #[test]
    fn filter_duplicate_and_stale_lists_do_not_execute_sql() {
        let mut page = ConnectionPage::default();
        page.start_new(Engine::Mysql);
        page.editor.as_mut().unwrap().name = "Atlas".into();
        page.editor.as_mut().unwrap().host = "db.example.test".into();
        page.editor.as_mut().unwrap().username = "app".into();
        let tasks = TaskBoard::new();
        let first = page.begin(&tasks);
        let second = page.begin(&tasks);
        let older = vec![record("old", "Old", Engine::Postgres)];
        let newer = vec![record("new", "Atlas", Engine::Mysql), record("redis", "Cache", Engine::Redis)];
        assert!(!page.accept_list(&tasks, first, older));
        assert!(page.rows.is_empty());
        assert!(page.accept_list(&tasks, second, newer));
        page.filter_text = "atlas".into();
        assert_eq!(page.filtered().len(), 1);
        page.filter_text.clear();
        page.filter_engine = Some(Engine::Redis);
        assert_eq!(page.filtered()[0].saved.engine, Engine::Redis);
        page.filter_engine = None;
        page.selected = Some("new".into());
        page.duplicate_selected();
        let copy = page.editor.unwrap();
        assert!(copy.id.is_none());
        assert!(!copy.save_password);
        assert!(copy.name.contains("copy"));
        assert!(!page.executed_sql);
    }

    #[test]
    fn an_open_failure_is_remembered_per_connection_until_it_connects() {
        let tasks = TaskBoard::new();
        let mut page = ConnectionPage { rows: vec![record("pg", "Atlas", Engine::Postgres)], selected: Some("pg".into()), ..Default::default() };
        assert!(!page.loaded, "清单还没被接受过");
        let token = page.begin(&tasks);
        assert!(page.note_open(&tasks, token, Err("connection refused".into())));
        assert_eq!(page.status.get("pg"), Some(&LinkStatus::Failed));
        assert_eq!(page.failures.get("pg").map(String::as_str), Some("connection refused"));
        // 状态栏消息被别的提示覆盖了也没关系：原因按连接留着。
        page.message_detail.clear();
        assert_eq!(page.failures.get("pg").map(String::as_str), Some("connection refused"));
    }

    fn record(id: &str, name: &str, engine: Engine) -> ConnectionRecord {
        let mut draft = sample_draft(engine);
        draft.name = name.into();
        let saved = normalize(&draft, id.into()).unwrap();
        ConnectionRecord { saved, ssh_host_key: None }
    }

    /// 启动台的“最近连接”只按持久化顺序取行：不认识的 id 跳过，绝不用列表前几条冒充。
    #[test]
    fn recent_rows_follow_the_persisted_order_and_skip_unknown_ids() {
        let page = ConnectionPage {
            rows: vec![
                record("a", "A", Engine::Postgres),
                record("b", "B", Engine::Mysql),
                record("c", "C", Engine::Sqlite),
            ],
            ..ConnectionPage::default()
        };
        let order = vec!["c".to_owned(), "deleted".to_owned(), "a".to_owned()];
        let rows = page.recent(&order, 5);
        assert_eq!(
            rows.iter().map(|row| row.saved.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "a"]
        );
        assert_eq!(page.recent(&order, 1).len(), 1, "上限只截断，不改顺序");
        assert!(page.recent(&[], 3).is_empty(), "从未使用过就没有最近连接");
    }

    fn stage(kind: StageKind, status: StageStatus, message: &str) -> crate::connections::stages::StageView {
        crate::connections::stages::StageView {
            kind,
            label: kind.label_zh().to_owned(),
            status,
            message: message.to_owned(),
            code: None,
        }
    }

    fn failed_report(code: Option<&str>, stages: Vec<crate::connections::stages::StageView>) -> TestReport {
        TestReport {
            status: TestStatus::Err,
            fail_stage: None,
            hint: None,
            summary: None,
            fingerprint: None,
            algorithm: None,
            stages,
            code: code.map(str::to_owned),
        }
    }

    /// 失败要落到用户下一步该改的字段上，原文留在 detail 里。
    #[test]
    fn failure_maps_to_the_field_the_user_should_fix() {
        let sqlite = sample_draft(Engine::Sqlite);
        let report = failed_report(
            Some(crate::connections::error_codes::CONN_SQLITE_FILE_MISSING),
            vec![stage(StageKind::OpenFile, StageStatus::Err, "打不开数据库文件 /tmp/x")],
        );
        let issue = field_issue_for_failure(&report, &sqlite).expect("file_path 错误");
        assert_eq!(issue.field, "file_path");
        assert_eq!(issue.key, "connections.status.error");
        assert!(issue.detail.contains("/tmp/x"));

        let remote = sample_draft(Engine::Postgres);
        let report = failed_report(
            Some(crate::connections::error_codes::CONN_NETWORK_FAILED),
            vec![stage(StageKind::Network, StageStatus::Err, "连不上 db.example.test:5432")],
        );
        assert_eq!(field_issue_for_failure(&report, &remote).unwrap().field, "host");

        let mut with_ca = sample_draft(Engine::Postgres);
        with_ca.ca_mode = CaMode::File;
        let report = failed_report(
            Some(crate::connections::error_codes::CONN_TLS_CA_MISSING),
            vec![stage(StageKind::Tls, StageStatus::Err, "CA 文件不存在")],
        );
        assert_eq!(field_issue_for_failure(&report, &with_ca).unwrap().field, "ca_file");

        let report = failed_report(
            Some(crate::connections::error_codes::CONN_PASSWORD_REQUIRED),
            vec![stage(StageKind::DatabaseAuthentication, StageStatus::Err, "请填写数据库密码")],
        );
        assert_eq!(field_issue_for_failure(&report, &remote).unwrap().field, "password");

        // 服务端拒绝凭据时分不清用户名还是密码：落在凭据组，不一律怪用户名（P3）。
        let report = failed_report(
            Some(crate::connections::error_codes::CONN_AUTH_FAILED),
            vec![stage(StageKind::DatabaseAuthentication, StageStatus::Err, "Access denied")],
        );
        assert_eq!(field_issue_for_failure(&report, &remote).unwrap().field, "credentials");
        let report = failed_report(
            Some(crate::connections::error_codes::CONN_REDIS_AUTH_FAILED),
            vec![stage(StageKind::DatabaseAuthentication, StageStatus::Err, "WRONGPASS")],
        );
        assert_eq!(field_issue_for_failure(&report, &remote).unwrap().field, "credentials");

        let mut with_ssh = sample_draft(Engine::Postgres);
        with_ssh.ssh_on = true;
        with_ssh.ssh_auth = SshAuthKind::Password;
        let report = failed_report(
            Some(crate::connections::error_codes::CONN_SSH_AUTH_FAILED),
            vec![stage(StageKind::SshAuthentication, StageStatus::Err, "SSH 密码被拒绝")],
        );
        assert_eq!(field_issue_for_failure(&report, &with_ssh).unwrap().field, "ssh_password");

        let report = TestReport {
            status: TestStatus::Ok,
            fail_stage: None,
            hint: None,
            summary: Some("连接正常".into()),
            fingerprint: None,
            algorithm: None,
            stages: vec![stage(StageKind::Network, StageStatus::Ok, "已连接")],
            code: None,
        };
        assert!(field_issue_for_failure(&report, &remote).is_none(), "成功没有字段错误");
    }

    fn ok_report() -> TestReport {
        TestReport {
            status: TestStatus::Ok,
            fail_stage: None,
            hint: None,
            summary: Some("连接正常".into()),
            fingerprint: None,
            algorithm: None,
            stages: vec![stage(StageKind::Network, StageStatus::Ok, "已连接")],
            code: None,
        }
    }

    fn editing_page(engine: Engine) -> ConnectionPage {
        let mut page = ConnectionPage::default();
        page.start_new(engine);
        let draft = page.editor.as_mut().unwrap();
        draft.host = "db.example.test".into();
        draft.username = "app".into();
        page
    }

    /// P3：更早一代的测试结果迟到时，连“正在测试”都不许碰——新那一次还在跑。
    #[test]
    fn a_late_result_never_clears_the_newer_running_indicator() {
        let tasks = TaskBoard::new();
        let mut page = editing_page(Engine::Postgres);
        let first = page.begin(&tasks);
        page.expect_test(1);
        let first_fingerprint = draft_fingerprint(&page.editor, &page.secrets);
        let second = page.begin(&tasks);
        page.expect_test(2);
        assert!(page.test_running());
        assert!(!page.note_test(&tasks, first, 1, first_fingerprint, Ok(ok_report())));
        assert!(page.test_running(), "更新的测试还在跑，指示不能被旧结果收走");
        assert_eq!(page.message_key, Some("connections.editor.test.running"));
        let second_fingerprint = draft_fingerprint(&page.editor, &page.secrets);
        assert!(page.note_test(&tasks, second, 2, second_fingerprint, Ok(ok_report())));
        assert!(!page.test_running());
        assert_eq!(page.message_key, Some("connections.editor.test.ok"));
    }

    /// 同一代里草稿已经改了：结果丢弃，指示收起，但不写消息或字段错误。
    #[test]
    fn an_edited_draft_drops_its_own_result_and_only_stops_the_indicator() {
        let tasks = TaskBoard::new();
        let mut page = editing_page(Engine::Postgres);
        let token = page.begin(&tasks);
        page.expect_test(7);
        let fingerprint = draft_fingerprint(&page.editor, &page.secrets);
        page.editor.as_mut().unwrap().host = "other.example.test".into();
        assert!(!page.note_test(&tasks, token, 7, fingerprint, Err("boom".into())));
        assert!(!page.test_running());
        assert_eq!(page.message_key, None);
        assert!(page.issues.is_empty());
    }

    /// 换数据库类型不清空表单：联网引擎之间沿用地址、用户、TLS 与 SSH；
    /// 端口等只在用户改过时才沿用；文件型与联网引擎之间只留名称和环境。
    #[test]
    fn switching_engine_keeps_what_still_applies() {
        let mut draft = blank_draft(Engine::Mysql);
        draft.name = "订单库".into();
        draft.host = "db.example.test".into();
        draft.username = "app".into();
        draft.database = "shop".into();
        draft.ssh_on = true;
        draft.ssh_host = "bastion.example.test".into();
        draft.ssh_user = "jump".into();
        draft.save_password = false;

        let maria = switch_engine(&draft, Engine::Mariadb);
        assert_eq!(maria.engine, Engine::Mariadb);
        assert_eq!((maria.name.as_str(), maria.host.as_str(), maria.username.as_str(), maria.database.as_str()), ("订单库", "db.example.test", "app", "shop"));
        assert!(maria.ssh_on && maria.ssh_host == "bastion.example.test" && maria.ssh_user == "jump");
        assert!(!maria.save_password, "是否保存密码是用户的选择，跟着走");

        // 没改过的端口换成新引擎的默认值；改过的端口留着。
        let pg = switch_engine(&draft, Engine::Postgres);
        assert_eq!(pg.port, default_port(Engine::Postgres));
        draft.port = "13306".into();
        assert_eq!(switch_engine(&draft, Engine::Postgres).port, "13306");

        // 没动过的环境与 TLS 模式跟新引擎的默认值走；改过的留着。
        let redis = switch_engine(&draft, Engine::Redis);
        assert_eq!((redis.env.as_str(), redis.tls_mode), ("local", TlsMode::Off));
        assert_eq!(redis.database, "0", "库名不是 Redis 的库索引");
        draft.env = "prod".into();
        draft.tls_mode = TlsMode::Require;
        let redis = switch_engine(&draft, Engine::Redis);
        assert_eq!((redis.env.as_str(), redis.tls_mode), ("prod", TlsMode::Require));

        // 文件型没有地址可沿用；换回来也不会带出一个文件路径当主机。
        let sqlite = switch_engine(&draft, Engine::Sqlite);
        assert_eq!((sqlite.name.as_str(), sqlite.env.as_str()), ("订单库", "prod"));
        assert!(sqlite.host.is_empty() && !sqlite.ssh_on && !sqlite.save_password);
        let mut file = blank_draft(Engine::Sqlite);
        file.file_path = "/tmp/notes.db".into();
        let back = switch_engine(&file, Engine::Postgres);
        assert!(back.host.is_empty() && back.file_path.is_empty());

        // 整串 MongoDB URI 不是别的引擎的主机名。
        let mut mongo = blank_draft(Engine::Mongodb);
        mongo.host = "mongodb+srv://cluster.example.test/shop".into();
        assert!(switch_engine(&mongo, Engine::Postgres).host.is_empty());

        // 编辑已有连接时换类型，仍是同一条连接。
        draft.id = Some("c1".into());
        assert_eq!(switch_engine(&draft, Engine::Postgres).id.as_deref(), Some("c1"));
        assert_eq!(switch_engine(&draft, Engine::Mysql), draft, "同一类型原样返回");
    }

    /// 只选了类型或环境的新草稿不算“填过”；填了任何字段或密码就算。
    #[test]
    fn an_untouched_new_draft_is_recognised() {
        let mut draft = blank_draft(Engine::Mysql);
        let secrets = Secrets::default();
        assert!(draft_is_untouched(&draft, &secrets));
        draft.env = "prod".into();
        assert!(draft_is_untouched(&draft, &secrets));
        assert!(!draft_is_untouched(&draft, &Secrets { password: "x".into(), ..Secrets::default() }));
        draft.host = "db".into();
        assert!(!draft_is_untouched(&draft, &secrets));
        let mut saved = blank_draft(Engine::Mysql);
        saved.id = Some("c1".into());
        assert!(!draft_is_untouched(&saved, &secrets), "已保存的连接走基线比较，不走这里");
    }

    /// 测试结论只属于被测的那份草稿：改了草稿就收起，正在跑的测试不受影响。
    #[test]
    fn touching_the_draft_retires_the_last_test_verdict() {
        let tasks = TaskBoard::new();
        let mut page = editing_page(Engine::Postgres);
        let token = page.begin(&tasks);
        page.expect_test(1);
        let fingerprint = draft_fingerprint(&page.editor, &page.secrets);
        assert!(page.note_test(&tasks, token, 1, fingerprint, Ok(ok_report())));
        assert!(page.test_report.is_some());
        assert_eq!(page.message_key, Some("connections.editor.test.ok"));
        page.draft_touched();
        assert!(page.test_report.is_none());
        assert_eq!(page.message_key, None);

        page.expect_test(2);
        page.draft_touched();
        assert_eq!(page.message_key, Some("connections.editor.test.running"), "指示由结果回来时收起");
    }

    /// 指纹只看内容：改字段、改机密、关表单都会得到不同的值。
    #[test]
    fn draft_fingerprint_tracks_edits_and_secrets() {
        let draft = sample_draft(Engine::Postgres);
        let secrets = Secrets::default();
        let base = draft_fingerprint(&Some(draft.clone()), &secrets);
        assert_eq!(base, draft_fingerprint(&Some(draft.clone()), &secrets));
        let mut edited = draft.clone();
        edited.host = "other.example.test".into();
        assert_ne!(base, draft_fingerprint(&Some(edited), &secrets));
        let typed = Secrets { password: "s3cret".into(), ..Secrets::default() };
        assert_ne!(base, draft_fingerprint(&Some(draft.clone()), &typed));
        assert_ne!(base, draft_fingerprint(&None, &secrets));
    }

    #[test]
    fn round_tripping_through_sqlite_preserves_network_fields_and_unencrypted_tls() {
        let mut draft = blank_draft(Engine::Mysql);
        draft.name = "demo-uat".into();
        draft.env = "test".into();
        draft.host = "192.168.92.210".into();
        draft.port = "3306".into();
        draft.username = "demo_user".into();
        draft.tls_mode = TlsMode::Off;
        let mut saved = normalize(&draft, "uat".into()).unwrap();
        saved.password_saved = true;

        let mut page = ConnectionPage {
            rows: vec![ConnectionRecord { saved, ssh_host_key: None }],
            selected: Some("uat".into()),
            ..ConnectionPage::default()
        };
        page.edit_selected();
        assert_eq!(page.editor.as_ref().unwrap().tls_mode, TlsMode::Off);

        // 误点 SQLite 再点回 MySQL：不丢失主机、用户名、端口或未加密设置。
        page.change_engine(Engine::Sqlite);
        assert_eq!(page.editor.as_ref().unwrap().engine, Engine::Sqlite);
        page.change_engine(Engine::Mysql);
        let restored = page.editor.as_ref().unwrap();
        assert_eq!(restored.host, "192.168.92.210");
        assert_eq!(restored.port, "3306");
        assert_eq!(restored.username, "demo_user");
        assert_eq!(restored.env, "test");
        assert_eq!(restored.tls_mode, TlsMode::Off);
        assert!(restored.save_password);
    }
}
