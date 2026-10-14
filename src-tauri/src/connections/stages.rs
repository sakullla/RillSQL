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
    pub fn tls_label(mode: TlsMode) -> &'static str {
        match mode {
            TlsMode::Verify => "加密连接并核对证书",
            TlsMode::Require | TlsMode::Off => "加密连接",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            StageKind::SshHostIdentity => "确认 SSH 跳板机身份",
            StageKind::SshAuthentication => "登录 SSH 跳板机",
            StageKind::Tunnel => "通过 SSH 跳板机打通到数据库的通道",
            StageKind::Network => "连接数据库服务器",
            StageKind::Tls => "加密连接",
            StageKind::DatabaseAuthentication => "登录数据库",
            StageKind::ServerVersion => "读取数据库版本",
            StageKind::OpenFile => "打开文件",
            StageKind::ReadSchema => "读取表结构清单",
            StageKind::ForeignKeys => "检查外键设置",
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
    /// 这一阶段没有执行，流水线继续，以便失败落在后面的阶段。
    NotAttempted,
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

    fn stage_label(&self, stage: StageKind) -> String {
        stage.label_zh().to_owned()
    }
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
                label: runner.stage_label(*stage),
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
                    label: runner.stage_label(*stage),
                    status: StageStatus::Ok,
                    message,
                });
            }
            Err(StageError::NotAttempted) => {
                views.push(StageView {
                    kind: *stage,
                    label: runner.stage_label(*stage),
                    status: StageStatus::Skipped,
                    message: "未执行".to_owned(),
                });
            }
            Err(StageError::WaitHostKey {
                algorithm: alg,
                fingerprint: fp,
            }) => {
                failed = true;
                status = TestStatus::Wait;
                fail_stage = Some(runner.stage_label(*stage));
                fingerprint = Some(fp.clone());
                algorithm = Some(alg);
                views.push(StageView {
                    kind: *stage,
                    label: runner.stage_label(*stage),
                    status: StageStatus::Wait,
                    message: format!("等待确认 SSH 服务器指纹 {fp}"),
                });
            }
            Err(StageError::Fail { message, hint: stage_hint }) => {
                failed = true;
                status = TestStatus::Err;
                fail_stage = Some(runner.stage_label(*stage));
                hint = Some(stage_hint);
                views.push(StageView {
                    kind: *stage,
                    label: runner.stage_label(*stage),
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
