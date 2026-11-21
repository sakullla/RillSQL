use std::path::Path;
use std::sync::Arc;

use crate::application::AppServices;
use crate::connections::model::{
    normalize, ConnectionDraft, EnvTag, FieldError, SavedConnection, ENV_NONE,
};
use crate::connections::profile_import::{parse_dbeaver, ProfileRow};
use crate::connections::store::{import_conflict, ImportConflict, ImportInsert};

#[derive(Clone, Debug, Default)]
pub(super) enum ImportState {
    #[default]
    Idle,
    Picking,
    Loading,
    Preview(ImportPreview),
    Saving,
    Report(ImportReport),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPreview {
    pub(super) rows: Vec<ImportRow>,
    existing: Arc<[SavedConnection]>,
    tags: Arc<[EnvTag]>,
    pub(super) editing: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ImportRow {
    pub source: ProfileRow,
    pub draft: Option<ConnectionDraft>,
    pub checked: bool,
    pub issues: Vec<FieldError>,
    pub conflict: Option<ImportConflict>,
}

impl ImportRow {
    pub fn eligible(&self) -> bool {
        self.draft.is_some() && self.issues.is_empty() && self.conflict.is_none()
    }
}

impl ImportPreview {
    pub(super) fn new(
        rows: Vec<ProfileRow>,
        existing: Vec<SavedConnection>,
        tags: Vec<EnvTag>,
    ) -> Self {
        let mut preview = Self {
            rows: rows
                .into_iter()
                .map(|source| ImportRow {
                    draft: source.draft.clone(),
                    source,
                    checked: false,
                    issues: Vec::new(),
                    conflict: None,
                })
                .collect(),
            existing: existing.into(),
            tags: tags.into(),
            editing: None,
        };
        preview.refresh();
        for row in &mut preview.rows {
            row.checked = row.eligible();
        }
        preview
    }

    pub fn refresh(&mut self) {
        let mut candidates = Vec::new();
        for row in &mut self.rows {
            row.conflict = None;
            if let Some(draft) = &row.draft {
                row.issues = row.source.issues(draft);
                if draft.env != ENV_NONE
                    && !self.tags.iter().any(|tag| {
                        tag.id == draft.env && (draft.env != "prod" || tag.tone == "prod")
                    })
                {
                    row.issues.push(FieldError {
                        field: "env".into(),
                        message: "环境标签不存在或已改变".into(),
                    });
                }
                if row.issues.is_empty() {
                    if let Ok(saved) = normalize(draft, String::new()) {
                        row.conflict = self
                            .existing
                            .iter()
                            .chain(&candidates)
                            .find_map(|other| import_conflict(&saved, other));
                        if row.conflict.is_none() {
                            candidates.push(saved);
                        }
                    }
                }
            }
            if !row.eligible() {
                row.checked = false;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImportField {
    Name,
    Host,
    Port,
    Database,
    Username,
    File,
    SshHost,
    SshPort,
    SshUser,
    SshKey,
    CaFile,
    ClientCert,
    ClientKey,
}

impl ImportField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "ui.conn.name",
            Self::Host => "ui.conn.host",
            Self::Port => "ui.conn.port",
            Self::Database => "ui.conn.database",
            Self::Username => "ui.conn.user",
            Self::File => "ui.conn.file",
            Self::SshHost => "ui.conn.sshHost",
            Self::SshPort => "connections.editor.ssh.port",
            Self::SshUser => "ui.conn.sshUser",
            Self::SshKey => "connections.editor.ssh.keyFile",
            Self::CaFile => "connections.editor.tls.caFile",
            Self::ClientCert => "connections.editor.tls.clientCert",
            Self::ClientKey => "connections.editor.tls.clientKey",
        }
    }

    pub fn get(self, draft: &ConnectionDraft) -> &str {
        match self {
            Self::Name => &draft.name,
            Self::Host => &draft.host,
            Self::Port => &draft.port,
            Self::Database => &draft.database,
            Self::Username => &draft.username,
            Self::File => &draft.file_path,
            Self::SshHost => &draft.ssh_host,
            Self::SshPort => &draft.ssh_port,
            Self::SshUser => &draft.ssh_user,
            Self::SshKey => &draft.ssh_key,
            Self::CaFile => &draft.ca_file,
            Self::ClientCert => &draft.client_cert,
            Self::ClientKey => &draft.client_key,
        }
    }

    pub fn set(self, draft: &mut ConnectionDraft, value: String) {
        match self {
            Self::Name => draft.name = value,
            Self::Host => draft.host = value,
            Self::Port => draft.port = value,
            Self::Database => draft.database = value,
            Self::Username => draft.username = value,
            Self::File => draft.file_path = value,
            Self::SshHost => draft.ssh_host = value,
            Self::SshPort => draft.ssh_port = value,
            Self::SshUser => draft.ssh_user = value,
            Self::SshKey => draft.ssh_key = value,
            Self::CaFile => draft.ca_file = value,
            Self::ClientCert => draft.client_cert = value,
            Self::ClientKey => draft.client_key = value,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ImportResult {
    Saved,
    Invalid(Vec<String>),
    Conflict(ImportConflict),
    Unchecked,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReport {
    pub(super) rows: Vec<ImportReportRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ImportReportRow {
    pub name: String,
    pub target: Option<String>,
    pub result: ImportResult,
}

pub(super) fn target_label(draft: &ConnectionDraft) -> String {
    let engine = crate::ui::kit::engine_label(draft.engine.as_str());
    if crate::app::views::connections::file_based(draft.engine) {
        return format!("{engine} · {}", draft.file_path);
    }
    let host = if draft.host.contains(':') {
        format!("[{}]", draft.host)
    } else {
        draft.host.clone()
    };
    let mut target = format!("{engine} · {host}:{}", draft.port);
    if !draft.database.is_empty() {
        target.push('/');
        target.push_str(&draft.database);
    }
    target
}

impl ImportReport {
    pub fn counts(&self) -> (usize, usize, usize) {
        let saved = self
            .rows
            .iter()
            .filter(|row| matches!(row.result, ImportResult::Saved))
            .count();
        let failed = self
            .rows
            .iter()
            .filter(|row| matches!(row.result, ImportResult::Failed))
            .count();
        (saved, self.rows.len() - saved - failed, failed)
    }
}

pub(super) async fn load(services: &AppServices, path: &Path) -> Result<ImportPreview, String> {
    let contents = tokio::fs::read(path)
        .await
        .map_err(|_| "无法读取 DBeaver 配置文件".to_owned())?;
    let rows = parse_dbeaver(&contents)?;
    let existing = services
        .list_connections()
        .await?
        .into_iter()
        .map(|record| record.saved)
        .collect();
    let tags = services.list_tags().await?;
    Ok(ImportPreview::new(rows, existing, tags))
}

pub(super) async fn save(services: &AppServices, mut preview: ImportPreview) -> ImportReport {
    preview.refresh();
    let mut report = ImportReport {
        rows: Vec::with_capacity(preview.rows.len()),
    };
    for row in preview.rows {
        let name = row
            .draft
            .as_ref()
            .map(|draft| draft.name.trim())
            .filter(|name| !name.is_empty())
            .unwrap_or(&row.source.name)
            .to_owned();
        let target = row.draft.as_ref().map(target_label);
        let result = if row.draft.is_none() {
            ImportResult::Invalid(row.source.blockers)
        } else if !row.issues.is_empty() {
            ImportResult::Invalid(row.issues.into_iter().map(|issue| issue.message).collect())
        } else if let Some(conflict) = row.conflict {
            ImportResult::Conflict(conflict)
        } else if let Some(draft) = row.draft.filter(|_| row.checked) {
            match services.insert_imported_connection(&draft).await {
                Ok(ImportInsert::Inserted) => ImportResult::Saved,
                Ok(ImportInsert::Conflict(conflict)) => ImportResult::Conflict(conflict),
                Ok(ImportInsert::MissingEnvironment) => {
                    ImportResult::Invalid(vec!["环境标签不存在或已改变".into()])
                }
                Err(crate::connections::ServiceError::Fields(fields)) => {
                    ImportResult::Invalid(fields.into_iter().map(|field| field.message).collect())
                }
                Err(_) => ImportResult::Failed,
            }
        } else {
            ImportResult::Unchecked
        };
        report.rows.push(ImportReportRow {
            name,
            target,
            result,
        });
    }
    report
}
