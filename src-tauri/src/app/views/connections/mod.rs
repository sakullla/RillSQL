//! 七种引擎的连接页状态。保存、测试、打开和导入导出都走应用服务，不走 Tauri command。

mod url;

use std::collections::BTreeMap;
use std::path::Path;

use crate::application::tasks::TaskToken;
use crate::application::{AppServices, TaskBoard, TaskOutcome};
use crate::connections::model::{
    CaMode, ConnectionDraft, Engine, FieldError, Secret, SshAuthKind, TlsMode, TlsSettings, SshSettings,
};
use crate::connections::{ConnectionRecord, OpenOutcome, SaveOutcome};
use crate::connections::stages::TestReport;

pub use url::{default_port, parse_connection_url, ParsedUrl};

const OWNER: &str = "connections";

#[derive(Clone)]
#[derive(Default)]
pub struct Secrets {
    pub password: String,
    pub ssh_password: String,
    pub ssh_passphrase: String,
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

#[derive(Clone, Debug)]
pub struct FieldIssue {
    pub field: String,
    pub key: &'static str,
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
                if !self.filter_env.is_empty() && row.saved.env != self.filter_env {
                    return false;
                }
                if needle.is_empty() {
                    return true;
                }
                let saved = &row.saved;
                let haystack = format!(
                    "{} {} {} {} {} {}",
                    saved.name,
                    saved.host.as_deref().unwrap_or(""),
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
        self.secrets = Secrets::default();
        self.issues.clear();
        self.confirm_delete = false;
        self.message_key = None;
    }

    pub fn edit_selected(&mut self) {
        let Some(id) = self.selected.clone() else { return };
        let Some(row) = self.rows.iter().find(|row| row.saved.id == id) else { return };
        self.editor = Some(draft_from_saved(&row.saved));
        self.secrets = Secrets::default();
        self.issues.clear();
        self.confirm_delete = false;
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
        if (parsed.engine == Engine::Sqlite || parsed.engine == Engine::Redis || parsed.engine == Engine::Mongodb)
            && draft.env == "dev" {
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
        self.secrets = Secrets::default();
        self.issues.clear();
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

    /// 环境筛选在“全部”、内置环境和自定义标签之间循环。
    pub fn cycle_env_filter(&mut self, tags: &[crate::connections::model::EnvTag]) {
        let mut options = vec![String::new(), "dev".into(), "test".into(), "prod".into(), "local".into()];
        for tag in tags {
            if !options.iter().any(|id| id == &tag.id) {
                options.push(tag.id.clone());
            }
        }
        let index = options.iter().position(|id| id == &self.filter_env).unwrap_or(0);
        self.filter_env = options[(index + 1) % options.len()].clone();
    }

    pub fn cycle_engine_filter(&mut self) {
        let engines = engines();
        self.filter_engine = match self.filter_engine {
            None => Some(engines[0]),
            Some(current) => {
                let index = engines.iter().position(|engine| *engine == current).unwrap_or(0);
                if index + 1 >= engines.len() { None } else { Some(engines[index + 1]) }
            }
        };
    }

    pub fn select_engine(&mut self, engine: Engine) {
        self.start_new(engine);
    }

    pub fn cycle_tls(&mut self) {
        let Some(draft) = self.editor.as_mut() else { return };
        draft.tls_mode = match draft.tls_mode {
            TlsMode::Off => TlsMode::Require,
            TlsMode::Require => TlsMode::Verify,
            TlsMode::Verify => TlsMode::Off,
        };
    }

    pub fn toggle_ssh(&mut self) {
        if let Some(draft) = self.editor.as_mut() {
            draft.ssh_on = !draft.ssh_on;
        }
    }

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

    pub fn cycle_env(&mut self) {
        let Some(draft) = self.editor.as_mut() else { return };
        draft.env = match draft.env.as_str() {
            "dev" => "test",
            "test" => "prod",
            "prod" => "local",
            _ => "dev",
        }
        .to_owned();
    }

    pub fn note_test(&mut self, tasks: &TaskBoard, token: TaskToken, report: Result<TestReport, String>) -> bool {
        let report = match tasks.accept(OWNER, token, report) {
            TaskOutcome::Current { value, .. } => value,
            TaskOutcome::Stale { .. } | TaskOutcome::Cancelled { .. } => return false,
        };
        match report {
            Ok(report) => {
                self.message_detail = report.summary.unwrap_or_default();
                self.message_key = Some(match report.status {
                    crate::connections::stages::TestStatus::Ok => "connections.editor.test.ok",
                    crate::connections::stages::TestStatus::Wait => "connections.editor.test.wait",
                    crate::connections::stages::TestStatus::Err => "connections.editor.test.failed",
                });
                if let Some(id) = &self.selected {
                    if report.status == crate::connections::stages::TestStatus::Err {
                        self.status.insert(id.clone(), LinkStatus::Failed);
                    }
                }
            }
            Err(detail) => {
                self.message_detail = detail;
                self.message_key = Some("connections.status.error");
                if let Some(id) = &self.selected {
                    self.status.insert(id.clone(), LinkStatus::Failed);
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
                self.status.insert(id, LinkStatus::Connected);
                self.message_key = Some("connections.status.connected");
                self.message_detail = outcome.report.summary.unwrap_or_default();
            }
            Ok(outcome) => {
                self.status.insert(id, LinkStatus::Failed);
                self.message_key = Some("connections.status.error");
                self.message_detail = outcome.report.summary.or(outcome.report.hint).unwrap_or_default();
            }
            Err(detail) => {
                self.status.insert(id, LinkStatus::Failed);
                self.message_key = Some("connections.status.error");
                self.message_detail = detail;
            }
        }
        self.executed_sql = false;
        true
    }
}

pub fn blank_draft(engine: Engine) -> ConnectionDraft {
    let local = matches!(engine, Engine::Sqlite | Engine::Redis | Engine::Mongodb);
    ConnectionDraft {
        id: None,
        name: String::new(),
        engine,
        env: if local { "local".into() } else { "dev".into() },
        host: String::new(),
        port: default_port(engine).to_owned(),
        database: if engine == Engine::Redis { "0".into() } else { String::new() },
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
        save_password: engine != Engine::Sqlite,
    }
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
        tls_mode: tls.as_ref().map(|item| item.mode).unwrap_or(if saved.engine == Engine::Sqlite { TlsMode::Off } else { TlsMode::Verify }),
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
        issues.push(FieldIssue { field: field.to_owned(), key });
    };
    if draft.engine == Engine::Sqlite {
        if draft.file_path.trim().is_empty() {
            push(&mut issues, "file_path", "connections.validate.chooseFile");
        }
        return issues;
    }
    if draft.host.trim().is_empty() {
        push(&mut issues, "host", "connections.validate.hostRequired");
    }
    let mongo_uri = draft.engine == Engine::Mongodb && Engine::host_is_mongo_uri(&draft.host);
    if !mongo_uri {
        let port = draft.port.trim().parse::<u16>().ok();
        if port.is_none_or(|port| port == 0) {
            push(&mut issues, "port", "connections.validate.portRange");
        }
    }
    if draft.username.trim().is_empty() && !matches!(draft.engine, Engine::Redis | Engine::Mongodb) {
        push(&mut issues, "username", "connections.validate.userRequired");
    }
    if draft.engine == Engine::Redis && !draft.database.trim().is_empty() && !draft.database.trim().chars().all(|ch| ch.is_ascii_digit()) {
        push(&mut issues, "database", "connections.validate.redisDbIndex");
    }
    if mongo_uri && draft.ssh_on {
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
    if draft.engine == Engine::Sqlite {
        return "connections.page.localFile";
    }
    match draft.tls_mode {
        TlsMode::Verify => "connections.page.tlsVerify",
        TlsMode::Require => "connections.page.tlsRequire",
        TlsMode::Off => "connections.page.tlsOff",
    }
}

pub async fn list_rows(services: &AppServices) -> Result<Vec<ConnectionRecord>, String> {
    services.list_connections().await
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

pub async fn test_row(services: &AppServices, id: &str, secrets: &Secrets) -> Result<TestReport, String> {
    services
        .test_connection(id, none_if_empty(&secrets.password), none_if_empty(&secrets.ssh_password), none_if_empty(&secrets.ssh_passphrase))
        .await
}

pub async fn open_row(services: &AppServices, id: &str, secrets: &Secrets) -> Result<OpenOutcome, String> {
    services
        .open_connection(id, none_if_empty(&secrets.password), none_if_empty(&secrets.ssh_password), none_if_empty(&secrets.ssh_passphrase))
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
    [Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlserver, Engine::Sqlite, Engine::Redis, Engine::Mongodb]
}

/// 给领域校验用的最小合法草稿。TLS/SSH 材料留在草稿里，密码不进这个结构。
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
    let _ = TlsSettings {
        mode: draft.tls_mode,
        server_name: draft.host.clone(),
        ca_mode: draft.ca_mode,
        ca_file: Some(draft.ca_file.clone()),
        client_cert: Some(draft.client_cert.clone()),
        client_key: Some(draft.client_key.clone()),
    };
    let _ = SshSettings {
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

    fn record(id: &str, name: &str, engine: Engine) -> ConnectionRecord {
        let mut draft = sample_draft(engine);
        draft.name = name.into();
        let saved = normalize(&draft, id.into()).unwrap();
        ConnectionRecord { saved, ssh_host_key: None }
    }
}
