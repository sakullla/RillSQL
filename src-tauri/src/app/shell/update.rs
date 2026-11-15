//! 应用内更新的界面状态：检查、发现新版本、下载、安装、待重启与失败。
//!
//! 这里只有纯状态与转换，网络与安装在视图层的后台线程里做，结果再回到这里落状态。
//! 自动检查静默进行：失败不打扰用户，只有用户手动检查时才把原因摆出来。

use crate::release::check::{InstallRoute, UpdateOffer};

/// 模型排给视图的更新任务：视图在后台线程执行，结果回到 `UpdateState`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateJob {
    Check,
    Download,
}

/// 启动时判定一次安装路线。调试构建可用 `RILLSQL_DEBUG_UPDATE_INSTALL_PATH` 指定一个可替换的文件，
/// 在开发机上走完整的下载、验签与安装流程；正式构建只看真实安装形态。
pub fn startup_route() -> InstallRoute {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("RILLSQL_DEBUG_UPDATE_INSTALL_PATH").map(std::path::PathBuf::from).filter(|path| path.is_absolute()) {
        return InstallRoute::InPlace(path);
    }
    crate::release::check::install_route()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStage {
    /// 还没检查过（或自动检查失败后静默回到这里）。
    #[default]
    Idle,
    Checking { manual: bool },
    /// 已是最新；记下检查时间（本地 HH:MM）。
    Latest { checked_at: String },
    /// 有新版本，细节在 `UpdateState::offer`。
    Available,
    Downloading { done: u64, total: Option<u64> },
    /// 已验签，正在交给安装器。
    Installing,
    /// 新版本已安装到位，重启后生效（macOS / AppImage）。
    Ready,
    Failed { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateState {
    pub stage: UpdateStage,
    pub offer: Option<UpdateOffer>,
    /// 本次启动是否已经排过自动检查：只排一次。
    pub auto_scheduled: bool,
}

impl UpdateState {
    pub fn busy(&self) -> bool {
        matches!(self.stage, UpdateStage::Checking { .. } | UpdateStage::Downloading { .. } | UpdateStage::Installing)
    }

    /// 开始一次检查；忙时（检查、下载、安装中）不重复开始。
    pub fn begin_check(&mut self, manual: bool) -> bool {
        if self.busy() || self.stage == UpdateStage::Ready {
            return false;
        }
        self.stage = UpdateStage::Checking { manual };
        true
    }

    /// 检查结果落地。自动检查的失败静默回到原状态；手动检查把原因留在卡片上。
    pub fn finish_check(&mut self, result: Result<Option<UpdateOffer>, String>, checked_at: String) {
        let manual = matches!(self.stage, UpdateStage::Checking { manual: true });
        self.stage = match result {
            Ok(Some(offer)) => {
                self.offer = Some(offer);
                UpdateStage::Available
            }
            Ok(None) => {
                self.offer = None;
                UpdateStage::Latest { checked_at }
            }
            Err(message) if manual => UpdateStage::Failed { message },
            // 自动检查失败：已知的新版本仍然保留，其余回到未检查。
            Err(_) if self.offer.is_some() => UpdateStage::Available,
            Err(_) => UpdateStage::Idle,
        };
    }

    /// 能否开始下载安装：有可安装的包、不在忙。
    pub fn can_install(&self, route: &InstallRoute) -> bool {
        !self.busy() && self.stage != UpdateStage::Ready && self.offer.as_ref().is_some_and(|offer| offer.installable(route))
    }

    pub fn begin_download(&mut self) -> bool {
        if self.offer.as_ref().and_then(|offer| offer.platform.as_ref()).is_none() || self.busy() {
            return false;
        }
        self.stage = UpdateStage::Downloading { done: 0, total: None };
        true
    }

    /// 下载进度只在下载阶段生效；迟到的进度不改写其它阶段。
    pub fn progress(&mut self, done: u64, total: Option<u64>) {
        if let UpdateStage::Downloading { done: current, total: known } = &mut self.stage {
            *current = done.max(*current);
            if total.is_some() {
                *known = total;
            }
        }
    }

    pub fn fail(&mut self, message: String) {
        self.stage = UpdateStage::Failed { message };
    }

    /// 下载百分比（总长未知时为 None）。
    pub fn percent(&self) -> Option<u8> {
        match self.stage {
            UpdateStage::Downloading { done, total: Some(total) } if total > 0 => Some(((done.min(total) * 100) / total) as u8),
            _ => None,
        }
    }

    /// 状态栏上要不要提示：有新版本、正在下载或等待重启时提示。
    pub fn announces(&self) -> bool {
        matches!(self.stage, UpdateStage::Available | UpdateStage::Downloading { .. } | UpdateStage::Installing | UpdateStage::Ready)
    }
}

/// 发布说明转成纯文本行：去掉 Markdown 的标题井号、列表符与转义，代码围栏不显示。
pub fn notes_lines(notes: &str) -> Vec<(bool, String)> {
    let mut lines = Vec::new();
    let mut fenced = false;
    for raw in notes.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if line.trim() == "---" {
            continue;
        }
        let text = line.replace("\\.", ".").replace("\\-", "-").replace("\\_", "_");
        if !fenced {
            if let Some(heading) = text.trim_start().strip_prefix('#') {
                lines.push((true, heading.trim_start_matches('#').trim().to_owned()));
                continue;
            }
            if let Some(item) = text.trim_start().strip_prefix("- ").or_else(|| text.trim_start().strip_prefix("* ")) {
                lines.push((false, format!("• {item}")));
                continue;
            }
        }
        lines.push((false, text));
    }
    // 连续空行收成一行，首尾空行去掉。
    lines.dedup_by(|a, b| a.1.is_empty() && b.1.is_empty());
    while lines.first().is_some_and(|line| line.1.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.1.is_empty()) {
        lines.pop();
    }
    lines
}

/// 把检查、下载与验签的原始错误归成用户看得懂的一句（文案键）；归不了类的保留原文。
pub fn error_key(raw: &str) -> Option<&'static str> {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("signature") || lower.contains("public key") {
        Some("update.badSignature")
    } else if lower.contains("http 404") {
        Some("update.noManifest")
    } else if lower.contains("timeout") || lower.contains("connection") || lower.contains("dns") || lower.contains("error sending request") {
        Some("update.offline")
    } else {
        None
    }
}

/// 字节数显示成 MB（一位小数）。
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::PlatformUpdate;
    use std::path::PathBuf;

    fn offer(platform: bool) -> UpdateOffer {
        UpdateOffer {
            version: "0.3.0".into(),
            notes: "### 修复\n\n- 修复 a\\.b\n".into(),
            pub_date: None,
            platform: platform.then(|| PlatformUpdate {
                target: "linux-x86_64".into(),
                version: "0.3.0".into(),
                url: "https://example.test/a.AppImage".into(),
                signature: "c2ln".into(),
                format: "appimage",
            }),
            page_url: "https://github.com/sakullla/RillSQL/releases/tag/v0.3.0".into(),
        }
    }

    #[test]
    fn manual_check_reports_failures_but_auto_check_stays_quiet() {
        let mut state = UpdateState::default();
        assert!(state.begin_check(false));
        assert!(!state.begin_check(true), "检查中不重复开始");
        state.finish_check(Err("timeout".into()), "10:00".into());
        assert_eq!(state.stage, UpdateStage::Idle, "自动检查失败不打扰");
        assert!(state.begin_check(true));
        state.finish_check(Err("timeout".into()), "10:00".into());
        assert_eq!(state.stage, UpdateStage::Failed { message: "timeout".into() });
        assert!(state.begin_check(true));
        state.finish_check(Ok(None), "10:05".into());
        assert_eq!(state.stage, UpdateStage::Latest { checked_at: "10:05".into() });
        assert!(!state.announces());
    }

    #[test]
    fn a_known_offer_survives_a_failed_background_recheck() {
        let mut state = UpdateState::default();
        state.begin_check(false);
        state.finish_check(Ok(Some(offer(true))), "10:00".into());
        assert_eq!(state.stage, UpdateStage::Available);
        assert!(state.announces());
        state.begin_check(false);
        state.finish_check(Err("offline".into()), "10:30".into());
        assert_eq!(state.stage, UpdateStage::Available);
        assert!(state.offer.is_some());
    }

    #[test]
    fn install_needs_a_package_and_an_in_place_route() {
        let image = InstallRoute::InPlace(PathBuf::from("/opt/RillSQL.AppImage"));
        let mut state = UpdateState { stage: UpdateStage::Available, offer: Some(offer(true)), auto_scheduled: true };
        assert!(state.can_install(&image));
        assert!(!state.can_install(&InstallRoute::Manual));
        state.offer = Some(offer(false));
        assert!(!state.can_install(&image), "清单里没有本机的包");
        assert!(!state.begin_download());
    }

    #[test]
    fn progress_counts_only_while_downloading() {
        let mut state = UpdateState { stage: UpdateStage::Available, offer: Some(offer(true)), auto_scheduled: true };
        state.progress(10, Some(100));
        assert_eq!(state.stage, UpdateStage::Available, "下载前的进度不生效");
        assert!(state.begin_download());
        state.progress(25, Some(100));
        state.progress(20, None);
        assert_eq!(state.stage, UpdateStage::Downloading { done: 25, total: Some(100) }, "进度不回退，总长保留");
        assert_eq!(state.percent(), Some(25));
        state.progress(200, Some(100));
        assert_eq!(state.percent(), Some(100));
        assert!(state.busy());
        assert!(!state.begin_check(true), "下载中不检查");
    }

    #[test]
    fn ready_state_stops_further_checks_and_installs() {
        let mut state = UpdateState { stage: UpdateStage::Ready, offer: Some(offer(true)), auto_scheduled: true };
        assert!(!state.begin_check(true));
        assert!(!state.can_install(&InstallRoute::InPlace(PathBuf::from("/x"))));
        assert!(state.announces());
    }

    #[test]
    fn release_notes_render_as_plain_lines() {
        let lines = notes_lines("本版本变更：\n\n\n### 其他\n\n- chore: 发布 v0\\.2\\.26\n\n---\n\n```sh\nxattr -dr x\n```\n");
        assert_eq!(
            lines,
            vec![
                (false, "本版本变更：".to_owned()),
                (false, String::new()),
                (true, "其他".to_owned()),
                (false, String::new()),
                (false, "• chore: 发布 v0.2.26".to_owned()),
                (false, String::new()),
                (false, "xattr -dr x".to_owned()),
            ]
        );
        assert_eq!(megabytes(31_894_008), "30.4 MB");
        assert_eq!(error_key("updater signature is not bound to this release version 9.9.9"), Some("update.badSignature"));
        assert_eq!(error_key("HTTP 404"), Some("update.noManifest"));
        assert_eq!(error_key("timeout"), Some("update.offline"));
        assert_eq!(error_key("install failed, current version kept: disk full"), None);
    }
}
