//! 关闭与更新安装共用同一组事实：运行中任务、未提交修改、未执行草稿分开判断。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseFacts {
    pub running: bool,
    pub uncommitted: bool,
    pub draft: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseKind {
    Allow,
    BlockedRunning,
    Confirm { uncommitted: bool, draft: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseScope {
    Tab,
    Window,
}

pub fn evaluate(facts: CloseFacts) -> CloseKind {
    if facts.running {
        return CloseKind::BlockedRunning;
    }
    if facts.uncommitted || facts.draft {
        return CloseKind::Confirm { uncommitted: facts.uncommitted, draft: facts.draft };
    }
    CloseKind::Allow
}

pub fn message_key(scope: CloseScope, kind: CloseKind) -> Option<&'static str> {
    match kind {
        CloseKind::Allow => None,
        CloseKind::BlockedRunning => Some("shell.update.busy"),
        CloseKind::Confirm { uncommitted, draft } => Some(match (scope, uncommitted, draft) {
            (CloseScope::Window, true, true) => "shell.closeGuard.windowBoth",
            (CloseScope::Window, false, true) => "shell.closeGuard.windowDraft",
            (CloseScope::Window, _, _) => "shell.closeGuard.windowCells",
            (CloseScope::Tab, true, true) => "shell.closeGuard.tabBoth",
            (CloseScope::Tab, false, true) => "shell.closeGuard.tabDraft",
            (CloseScope::Tab, _, _) => "shell.closeGuard.tabCells",
        }),
    }
}

/// 更新安装不能绕过关闭守卫。允许关闭才允许安装。
pub fn update_block_key(facts: CloseFacts) -> Option<&'static str> {
    match evaluate(facts) {
        CloseKind::Allow => None,
        CloseKind::BlockedRunning => Some("shell.update.busy"),
        CloseKind::Confirm { uncommitted: true, draft: false } => Some("shell.update.pending"),
        CloseKind::Confirm { uncommitted: false, draft: true } => Some("shell.closeGuard.windowDraft"),
        CloseKind::Confirm { uncommitted: true, draft: true } => Some("shell.closeGuard.windowBoth"),
        CloseKind::Confirm { .. } => Some("shell.update.pending"),
    }
}

pub fn draft_pending(kind: &str, draft: &str, last_executed: Option<&str>) -> bool {
    kind == "query" && !draft.trim().is_empty() && Some(draft) != last_executed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_uncommitted_and_draft_stay_distinct() {
        assert_eq!(evaluate(CloseFacts { running: true, uncommitted: true, draft: true }), CloseKind::BlockedRunning);
        assert_eq!(
            evaluate(CloseFacts { running: false, uncommitted: true, draft: false }),
            CloseKind::Confirm { uncommitted: true, draft: false }
        );
        assert_eq!(
            evaluate(CloseFacts { running: false, uncommitted: false, draft: true }),
            CloseKind::Confirm { uncommitted: false, draft: true }
        );
        assert_eq!(
            message_key(CloseScope::Window, CloseKind::Confirm { uncommitted: true, draft: true }),
            Some("shell.closeGuard.windowBoth")
        );
        assert_eq!(message_key(CloseScope::Tab, CloseKind::Confirm { uncommitted: false, draft: true }), Some("shell.closeGuard.tabDraft"));
        assert_eq!(update_block_key(CloseFacts { running: true, uncommitted: false, draft: false }), Some("shell.update.busy"));
        assert_eq!(update_block_key(CloseFacts { running: false, uncommitted: true, draft: false }), Some("shell.update.pending"));
        assert_eq!(update_block_key(CloseFacts { running: false, uncommitted: false, draft: true }), Some("shell.closeGuard.windowDraft"));
        assert_eq!(update_block_key(CloseFacts { running: false, uncommitted: false, draft: false }), None);
        assert!(!draft_pending("query", "select 1", Some("select 1")));
        assert!(draft_pending("query", "select 1", None));
        assert!(!draft_pending("query", "  ", None));
        assert!(!draft_pending("table", "select 1", None));
    }
}
