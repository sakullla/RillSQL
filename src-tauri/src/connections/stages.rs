use serde::{Deserialize, Serialize};

use crate::connections::error_codes;
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
    /// 稳定错误码（T6-A2/R6）：失败/等待阶段携带，前端按码取字典文案；成功/跳过为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
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
    /// 失败/等待阶段的稳定错误码（T6-A2/R6）：前端先查码取字典文案，未知或缺失回退原文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageError {
    Fail { code: &'static str, message: String, hint: String },
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
    let mut report_code = None;
    for stage in stages {
        if failed {
            views.push(StageView {
                kind: *stage,
                label: runner.stage_label(*stage),
                status: StageStatus::Skipped,
                message: "未执行".to_owned(),
                code: None,
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
                    code: None,
                });
            }
            Err(StageError::NotAttempted) => {
                views.push(StageView {
                    kind: *stage,
                    label: runner.stage_label(*stage),
                    status: StageStatus::Skipped,
                    message: "未执行".to_owned(),
                    code: None,
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
                report_code = Some(error_codes::SSH_HOST_KEY_WAIT.to_owned());
                views.push(StageView {
                    kind: *stage,
                    label: runner.stage_label(*stage),
                    status: StageStatus::Wait,
                    message: format!("等待确认 SSH 服务器指纹 {fp}"),
                    code: Some(error_codes::SSH_HOST_KEY_WAIT.to_owned()),
                });
            }
            Err(StageError::Fail { code, message, hint: stage_hint }) => {
                failed = true;
                status = TestStatus::Err;
                fail_stage = Some(runner.stage_label(*stage));
                hint = Some(stage_hint);
                report_code = Some(code.to_owned());
                views.push(StageView {
                    kind: *stage,
                    label: runner.stage_label(*stage),
                    status: StageStatus::Err,
                    message,
                    code: Some(code.to_owned()),
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
        code: report_code,
    }
}

/// 带稳定错误码的失败（T6-A2/R6）：code 供前端取字典文案，中文 message/hint 保持不变。
pub fn fail_code(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> StageError {
    debug_assert!(
        error_codes::is_valid_code(code),
        "错误码必须形如 domain.name：{code}"
    );
    StageError::Fail {
        code,
        message: message.into(),
        hint: hint.into(),
    }
}

/// 兜底失败（默认码 conn.stage_failed）：生产路径已全部改用 fail_code 显式赋码；
/// 该入口保留给测试桩与后续临时失败点，前端对未知码回退原文。
#[cfg_attr(not(test), allow(dead_code))]
pub fn fail(message: impl Into<String>, hint: impl Into<String>) -> StageError {
    fail_code(error_codes::CONN_STAGE_FAILED, message, hint)
}

#[cfg(test)]
mod error_code_tests {
    use super::*;
    use crate::connections::error_codes;

    struct FailingRunner {
        error: StageError,
    }

    #[async_trait::async_trait]
    impl StageRunner for FailingRunner {
        async fn run_stage(&mut self, _stage: StageKind) -> Result<String, StageError> {
            Err(match self.error.clone() {
                StageError::Fail { code, message, hint } => StageError::Fail { code, message, hint },
                other => other,
            })
        }
    }

    /// T6-A2：报告携带失败阶段的稳定错误码；中文 message/hint 保持原文不变。
    #[tokio::test]
    async fn failed_report_carries_stage_code_and_keeps_chinese_text() {
        let error = fail_code(
            error_codes::CONN_PASSWORD_REQUIRED,
            "请填写数据库密码",
            "凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。",
        );
        let runner = FailingRunner { error };
        let report = execute_stages(&[StageKind::DatabaseAuthentication], &mut { runner }).await;
        assert_eq!(report.status, TestStatus::Err);
        assert_eq!(report.code.as_deref(), Some(error_codes::CONN_PASSWORD_REQUIRED));
        assert_eq!(report.hint.as_deref(), Some("凭据库没有可用密码，本次也没有输入。没有把空密码写入配置。"));
        let stage = &report.stages[0];
        assert_eq!(stage.code.as_deref(), Some(error_codes::CONN_PASSWORD_REQUIRED));
        assert_eq!(stage.message, "请填写数据库密码");
    }

    /// T6-A2：等待主机指纹的报告携带 ssh.host_key_wait。
    #[tokio::test]
    async fn wait_host_key_report_carries_wait_code() {
        let runner = FailingRunner {
            error: StageError::WaitHostKey {
                algorithm: "ssh-ed25519".to_owned(),
                fingerprint: "SHA256:abc".to_owned(),
            },
        };
        let report = execute_stages(&[StageKind::SshHostIdentity], &mut { runner }).await;
        assert_eq!(report.status, TestStatus::Wait);
        assert_eq!(report.code.as_deref(), Some(error_codes::SSH_HOST_KEY_WAIT));
    }

    /// T6-A2：成功报告不带码。
    #[tokio::test]
    async fn ok_report_has_no_code() {
        struct OkRunner;
        #[async_trait::async_trait]
        impl StageRunner for OkRunner {
            async fn run_stage(&mut self, _stage: StageKind) -> Result<String, StageError> {
                Ok("完成".to_owned())
            }
        }
        let report = execute_stages(&[StageKind::Network], &mut { OkRunner }).await;
        assert_eq!(report.status, TestStatus::Ok);
        assert_eq!(report.code, None);
        assert!(report.stages.iter().all(|stage| stage.code.is_none()));
    }

    /// T6-A2：错误码稳定性——全部公开码满足命名形状，且互不相同（改码即破坏前端映射，此测试兜底）。
    #[test]
    fn all_registered_codes_are_well_formed_and_unique() {
        let codes = [
            error_codes::CONN_STAGE_FAILED,
            error_codes::CONN_AUTH_FAILED,
            error_codes::CONN_REDIS_AUTH_FAILED,
            error_codes::CONN_NETWORK_FAILED,
            error_codes::CONN_TLS_TERMINATED,
            error_codes::CONN_TLS_DOWNGRADE_REFUSED,
            error_codes::CONN_TLS_CA_MISSING,
            error_codes::CONN_TLS_CA_UNREADABLE,
            error_codes::CONN_TLS_CERT_MISSING,
            error_codes::CONN_TLS_CERT_UNREADABLE,
            error_codes::CONN_TLS_NOT_CONFIGURED,
            error_codes::CONN_VERSION_READ_FAILED,
            error_codes::CONN_SCHEMA_READ_FAILED,
            error_codes::CONN_SQLITE_FILE_MISSING,
            error_codes::CONN_SQLITE_OPEN_FAILED,
            error_codes::CONN_FOREIGN_KEYS_READ_FAILED,
            error_codes::CONN_SSH_AUTH_FAILED,
            error_codes::CONN_SSH_KEY_UNREADABLE,
            error_codes::CONN_SSH_NOT_CONFIGURED,
            error_codes::CONN_TUNNEL_FAILED,
            error_codes::CONN_TUNNEL_NOT_READY,
            error_codes::CONN_HOST_KEY_CHANGED,
            error_codes::CONN_HOST_KEY_INCONSISTENT,
            error_codes::CONN_ROUTE_RESOLVE_FAILED,
            error_codes::CONN_PASSWORD_REQUIRED,
            error_codes::CONN_SQLITE_NOT_REMOTE,
            error_codes::CONN_NOT_STARTED,
            error_codes::CONN_PROBE_FAILED,
            error_codes::CONN_VERSION_UNSUPPORTED,
            error_codes::CONN_UNKNOWN_VERSION,
            error_codes::SSH_HOST_KEY_WAIT,
            error_codes::IMPORT_PASSPHRASE_REQUIRED,
            error_codes::IMPORT_BAD_PASSPHRASE,
            error_codes::IMPORT_UNRECOGNIZED,
            error_codes::DUMP_ENGINE_UNSUPPORTED,
            error_codes::DUMP_CONNECTION_MISMATCH,
            error_codes::DUMP_SESSION_GAP,
            error_codes::DUMP_SESSION_GAP_MAIN,
            error_codes::DUMP_DATABASE_REQUIRED,
            error_codes::MIGRATE_NOT_QUERYABLE,
            error_codes::MIGRATE_REDIS_NOT_TARGET,
            error_codes::MIGRATE_MONGO_NOT_TARGET,
            error_codes::MIGRATE_TARGET_NOT_TABLE,
            error_codes::MIGRATE_NO_MAPPED_COLUMNS,
            error_codes::MIGRATE_DUPLICATE_MAPPING,
            error_codes::MIGRATE_SOURCE_READ_FAILED,
            error_codes::MIGRATE_TARGET_READ_FAILED,
            error_codes::MIGRATE_TABLE_MISSING,
        ];
        assert!(!codes.is_empty());
        let mut seen = std::collections::HashSet::new();
        for code in codes {
            assert!(error_codes::is_valid_code(code), "错误码形状不合规：{code}");
            assert!(seen.insert(code), "错误码重复注册：{code}");
        }
    }

    /// T6-A2：纯字符串错误的加码约定——前缀可被前端识别剥离，中文原文保持。
    #[test]
    fn coded_string_prefix_is_parseable_and_message_intact() {
        let text = error_codes::coded_string(error_codes::IMPORT_PASSPHRASE_REQUIRED, "这份文件带口令加密，请输入导出时设置的口令。");
        assert!(text.starts_with("[import.passphrase_required] "));
        assert!(text.contains("带口令加密"));
        assert!(text.ends_with("请输入导出时设置的口令。"));
    }
}
