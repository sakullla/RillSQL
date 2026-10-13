use serde::{Deserialize, Serialize};

use crate::connections::model::{Engine, SavedConnection, TlsMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    SshHostIdentity,
    SshAuthentication,
    Tunnel,
    Network,
    Tls,
    DatabaseAuthentication,
    ServerVersion,
    OpenFile,
    ReadSchema,
    ForeignKeys,
}

impl StageKind {
    pub fn label_zh(self) -> &'static str {
        match self {
            StageKind::SshHostIdentity => "SSH 主机身份",
            StageKind::SshAuthentication => "SSH 认证",
            StageKind::Tunnel => "建立隧道",
            StageKind::Network => "网络连接",
            StageKind::Tls => "TLS 握手与证书验证",
            StageKind::DatabaseAuthentication => "数据库认证",
            StageKind::ServerVersion => "读取服务器版本",
            StageKind::OpenFile => "打开文件",
            StageKind::ReadSchema => "读取 schema",
            StageKind::ForeignKeys => "检查 foreign_keys",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Ok,
    Err,
    Skipped,
    Wait,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StageView {
    pub kind: StageKind,
    pub label: String,
    pub status: StageStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Ok,
    Err,
    Wait,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub status: TestStatus,
    pub fail_stage: Option<String>,
    pub hint: Option<String>,
    pub summary: Option<String>,
    pub fingerprint: Option<String>,
    pub algorithm: Option<String>,
    pub stages: Vec<StageView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageError {
    Fail { message: String, hint: String },
    WaitHostKey { algorithm: String, fingerprint: String },
}

pub fn planned_stages(config: &SavedConnection) -> Vec<StageKind> {
    if config.engine == Engine::Sqlite {
        return vec![StageKind::OpenFile, StageKind::ReadSchema, StageKind::ForeignKeys];
    }
    let mut stages = Vec::new();
    if config.ssh.is_some() {
        stages.extend([
            StageKind::SshHostIdentity,
            StageKind::SshAuthentication,
            StageKind::Tunnel,
        ]);
    } else {
        stages.push(StageKind::Network);
    }
    let tls_mode = config.tls.as_ref().map(|tls| tls.mode).unwrap_or(TlsMode::Off);
    if tls_mode != TlsMode::Off {
        stages.push(StageKind::Tls);
    }
    stages.push(StageKind::DatabaseAuthentication);
    stages.push(StageKind::ServerVersion);
    stages
}

#[async_trait::async_trait]
pub trait StageRunner: Send {
    async fn run_stage(&mut self, stage: StageKind) -> Result<String, StageError>;
}

pub async fn execute_stages(stages: &[StageKind], runner: &mut (dyn StageRunner + Send)) -> TestReport {
    let mut views = Vec::with_capacity(stages.len());
    let mut failed = false;
    let mut fail_stage = None;
    let mut hint = None;
    let mut status = TestStatus::Ok;
    let mut fingerprint = None;
    let mut algorithm = None;
    let mut summary = None;
    for stage in stages {
        if failed {
            views.push(StageView {
                kind: *stage,
                label: stage.label_zh().to_owned(),
                status: StageStatus::Skipped,
                message: "未执行".to_owned(),
            });
            continue;
        }
        match runner.run_stage(*stage).await {
            Ok(message) => {
                if *stage == StageKind::ServerVersion || *stage == StageKind::ForeignKeys {
                    summary = Some(message.clone());
                }
                views.push(StageView {
                    kind: *stage,
                    label: stage.label_zh().to_owned(),
                    status: StageStatus::Ok,
                    message,
                });
            }
            Err(StageError::WaitHostKey {
                algorithm: alg,
                fingerprint: fp,
            }) => {
                failed = true;
                status = TestStatus::Wait;
                fail_stage = Some(stage.label_zh().to_owned());
                fingerprint = Some(fp.clone());
                algorithm = Some(alg);
                views.push(StageView {
                    kind: *stage,
                    label: stage.label_zh().to_owned(),
                    status: StageStatus::Wait,
                    message: format!("等待确认主机指纹 {fp}"),
                });
            }
            Err(StageError::Fail { message, hint: stage_hint }) => {
                failed = true;
                status = TestStatus::Err;
                fail_stage = Some(stage.label_zh().to_owned());
                hint = Some(stage_hint);
                views.push(StageView {
                    kind: *stage,
                    label: stage.label_zh().to_owned(),
                    status: StageStatus::Err,
                    message,
                });
            }
        }
    }
    TestReport {
        status,
        fail_stage,
        hint,
        summary: if status == TestStatus::Ok { summary } else { None },
        fingerprint,
        algorithm,
        stages: views,
    }
}

pub fn fail(message: impl Into<String>, hint: impl Into<String>) -> StageError {
    StageError::Fail {
        message: message.into(),
        hint: hint.into(),
    }
}
