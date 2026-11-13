//! 新建/编辑连接的全局模态生命周期。
//!
//! 草稿、机密与字段错误仍然只有一份（`ConnectionPage`），这里只持有"弹窗开着吗、
//! 关闭后还给谁、这一次测试属于哪一代"这类生命周期。启动台、命令面板、对象树空态和
//! 连接行操作都调用同一入口，因此不存在第二个编辑面。

use crate::app::model::FocusTarget;
use crate::app::views::connections::ConnectionPage;
use crate::connections::model::{CaMode, Engine, SshAuthKind, TlsMode};

use super::model::DraftField;

/// 关闭后把真实焦点还给哪个控件。
///
/// 列表行按 S12.3 由容器持有焦点、行本身不是 Tab 停靠，所以行触发时还给列表容器；
/// 静态主入口（启动台"新建"、对象树空态的"新建"）还给按钮自己。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModalControl {
    LauncherNew,
    SidebarNew,
}

/// 模态内部的一个 Tab 停靠点。字段用的是输入框自己的焦点，页脚按钮各有一个焦点句柄。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalStop {
    Field(DraftField),
    Cancel,
    Test,
    Save,
    SaveConnect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionModal {
    pub open: bool,
    pub origin: FocusTarget,
    pub control: Option<ModalControl>,
    /// 触发控件 id：只用于无障碍描述与测试，焦点归还走 `control`/列表容器。
    pub trigger: Option<String>,
    /// 每次发起测试自增。过期结果只能收回自己那一代，不能关掉更新的"正在测试"。
    pub test_generation: u64,
    pub test_running: bool,
    /// 这次保存成功后是否续接连接（"保存并连接"）。普通保存永远为 false。
    pub save_and_connect: bool,
    /// 打开后下一帧把焦点交给首个字段。
    pub focus_first_pending: bool,
    /// 当前 Tab 停靠下标；`stops` 每次按草稿形状重算，下标越界按 0 处理。
    pub focus: usize,
    /// 上一次关闭时该还焦的控件；视图取走后清空，保证只还焦一次。
    pub last_control: Option<ModalControl>,
    /// 视图下一帧把焦点还给来源区域。
    pub restore_origin: bool,
}

impl Default for ConnectionModal {
    fn default() -> Self {
        Self {
            open: false,
            origin: FocusTarget::Sidebar,
            control: None,
            trigger: None,
            test_generation: 0,
            test_running: false,
            save_and_connect: false,
            focus_first_pending: false,
            focus: 0,
            last_control: None,
            restore_origin: false,
        }
    }
}

impl ConnectionModal {
    /// 打开：来源区域与触发控件都记下来，关闭时按它们还焦。
    pub fn open_from(&mut self, origin: FocusTarget, control: Option<ModalControl>, trigger: impl Into<String>) {
        self.open = true;
        self.origin = origin;
        self.control = control;
        self.trigger = Some(trigger.into());
        self.save_and_connect = false;
        self.focus_first_pending = true;
        self.focus = 0;
        self.test_running = false;
        self.test_generation = self.test_generation.saturating_add(1);
    }

    /// 关闭并交还焦点归属；重复关闭不产生第二次还焦。
    pub fn close(&mut self) -> Option<(FocusTarget, Option<ModalControl>)> {
        if !self.open {
            return None;
        }
        self.open = false;
        self.save_and_connect = false;
        self.test_running = false;
        self.focus_first_pending = false;
        let origin = self.origin;
        let control = self.control.take();
        self.trigger = None;
        self.last_control = control.clone();
        self.restore_origin = true;
        Some((origin, control))
    }

    /// 发起一次测试：拿到本次的代号。只有代号仍是最新的那次结果才能收回指示。
    pub fn begin_test(&mut self) -> u64 {
        self.test_generation = self.test_generation.saturating_add(1);
        self.test_running = true;
        self.test_generation
    }

    /// 结果回来（成功、失败或过期）：代号仍是最新才清"正在测试"，返回是否仍是当前。
    pub fn end_test(&mut self, generation: u64) -> bool {
        if generation != self.test_generation {
            return false;
        }
        self.test_running = false;
        true
    }

    pub fn is_current_test(&self, generation: u64) -> bool {
        self.test_generation == generation
    }

    /// 模态内的 Tab 序列：按引擎与安全选项决定哪些字段真的在屏幕上。
    pub fn stops(page: &ConnectionPage) -> Vec<ModalStop> {
        let Some(draft) = page.editor.as_ref() else {
            return Vec::new();
        };
        let mut stops = vec![ModalStop::Field(DraftField::Url), ModalStop::Field(DraftField::Name)];
        if draft.engine == Engine::Sqlite {
            stops.push(ModalStop::Field(DraftField::File));
        } else {
            stops.push(ModalStop::Field(DraftField::Host));
            stops.push(ModalStop::Field(DraftField::Port));
            stops.push(ModalStop::Field(DraftField::Username));
            stops.push(ModalStop::Field(DraftField::Password));
            stops.push(ModalStop::Field(DraftField::Database));
            if draft.engine == Engine::Mongodb {
                stops.push(ModalStop::Field(DraftField::Replica));
            }
            if draft.tls_mode == TlsMode::Verify {
                if draft.ca_mode == CaMode::File {
                    stops.push(ModalStop::Field(DraftField::CaFile));
                }
                stops.push(ModalStop::Field(DraftField::ClientCert));
                stops.push(ModalStop::Field(DraftField::ClientKey));
            }
            if draft.ssh_on {
                stops.push(ModalStop::Field(DraftField::SshHost));
                stops.push(ModalStop::Field(DraftField::SshPort));
                stops.push(ModalStop::Field(DraftField::SshUser));
                if draft.ssh_auth == SshAuthKind::Key {
                    stops.push(ModalStop::Field(DraftField::SshKey));
                    stops.push(ModalStop::Field(DraftField::Passphrase));
                } else {
                    stops.push(ModalStop::Field(DraftField::SshPassword));
                }
            }
        }
        stops.push(ModalStop::Cancel);
        stops.push(ModalStop::Test);
        stops.push(ModalStop::Save);
        stops.push(ModalStop::SaveConnect);
        stops
    }

    /// Tab / Shift+Tab：只在模态自己的序列里循环，不逃到下面的页面。
    pub fn step(&mut self, page: &ConnectionPage, forward: bool) -> Option<ModalStop> {
        let stops = Self::stops(page);
        if stops.is_empty() {
            return None;
        }
        let len = stops.len();
        let current = self.focus.min(len - 1);
        let next = if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.focus = next;
        stops.get(next).copied()
    }

    /// 首个字段：打开或校验失败后把焦点交给它，而不是留在页脚。
    pub fn first_stop(page: &ConnectionPage) -> Option<ModalStop> {
        Self::stops(page).into_iter().next()
    }
}

/// 按需连接要续接的动作。鼠标和键盘走同一条路径，因此这里只描述"连上之后做什么"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectAction {
    /// 展开对象树（chevron）。
    Expand,
    /// 连上后进入工作区（启动台/命令搜索里的连接行双击）。
    OpenWorkspace,
    /// 打开数据页（对象树"打开数据"、双击表）。
    OpenData,
    /// 打开结构页。
    OpenStructure,
    /// 在此对象/连接上新建查询。
    NewQuery,
}

/// 一个等待连接的续接动作。`spec` 是触发时的对象规格：连接成功后按它重放，
/// 不重新读当前选择，避免连接期间换了焦点就打开别的对象。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectIntent {
    pub connection_id: String,
    pub action: ConnectAction,
    pub spec: Option<String>,
}

impl ConnectIntent {
    pub fn new(connection_id: impl Into<String>, action: ConnectAction, spec: Option<String>) -> Self {
        Self { connection_id: connection_id.into(), action, spec }
    }

    /// 连接成功后要执行的命令；`Expand` 只改树状态，没有命令。
    pub fn command(&self) -> Option<&'static str> {
        match self.action {
            ConnectAction::Expand => None,
            ConnectAction::OpenWorkspace => Some("activity-workspace"),
            ConnectAction::OpenData => Some("tree-open-data"),
            ConnectAction::OpenStructure => Some("open-structure"),
            ConnectAction::NewQuery => Some("tree-new-query"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(engine: Engine) -> ConnectionPage {
        let mut page = ConnectionPage::default();
        page.start_new(engine);
        page
    }

    #[test]
    fn tab_order_stays_inside_the_modal_and_wraps() {
        let page = page(Engine::Postgres);
        let stops = ConnectionModal::stops(&page);
        assert_eq!(stops.first(), Some(&ModalStop::Field(DraftField::Url)));
        assert_eq!(stops.last(), Some(&ModalStop::SaveConnect));
        let mut modal = ConnectionModal::default();
        modal.open_from(FocusTarget::Sidebar, None, "explorer-new");
        let mut seen = Vec::new();
        for _ in 0..stops.len() {
            seen.push(modal.step(&page, true).unwrap());
        }
        let mut expected = stops.clone();
        // 起点是首个字段，第一次 Tab 落到第二个停靠点，因此整轮是左旋一位的完整序列。
        expected.rotate_left(1);
        assert_eq!(seen, expected, "一整轮就是屏幕上全部停靠点，不多不少");
        assert_eq!(modal.step(&page, true).unwrap(), stops[1], "走完一轮后从首个字段继续往后");
        assert_eq!(modal.step(&page, false).unwrap(), stops[0], "反向同样成环");
    }

    #[test]
    fn stops_follow_the_shape_of_the_form() {
        let sqlite = ConnectionModal::stops(&page(Engine::Sqlite));
        assert!(sqlite.iter().all(|stop| *stop != ModalStop::Field(DraftField::Host)), "文件型没有 host");
        assert!(sqlite.contains(&ModalStop::Field(DraftField::File)));

        let mut remote = page(Engine::Postgres);
        remote.editor.as_mut().unwrap().ssh_on = true;
        remote.editor.as_mut().unwrap().ssh_auth = SshAuthKind::Password;
        let stops = ConnectionModal::stops(&remote);
        assert!(stops.contains(&ModalStop::Field(DraftField::SshPassword)));
        assert!(!stops.contains(&ModalStop::Field(DraftField::SshKey)), "密码认证没有私钥字段");

        remote.editor.as_mut().unwrap().tls_mode = TlsMode::Verify;
        remote.editor.as_mut().unwrap().ca_mode = CaMode::File;
        remote.editor.as_mut().unwrap().ssh_on = false;
        assert!(ConnectionModal::stops(&remote).contains(&ModalStop::Field(DraftField::CaFile)));
    }

    /// P3：老结果只能收回自己那一代；更新的测试仍在跑时，旧的迟到结果不能把指示关掉。
    #[test]
    fn a_stale_test_result_never_clears_the_newer_indicator() {
        let mut modal = ConnectionModal::default();
        let first = modal.begin_test();
        assert!(modal.test_running);
        let second = modal.begin_test();
        assert_ne!(first, second);
        assert!(!modal.end_test(first), "旧代号不是当前代号");
        assert!(modal.test_running, "新那一次还在跑，指示不能被旧结果收走");
        assert!(modal.is_current_test(second));
        assert!(modal.end_test(second));
        assert!(!modal.test_running);
    }

    #[test]
    fn closing_returns_focus_once_and_clears_pending_state() {
        let mut modal = ConnectionModal::default();
        modal.open_from(FocusTarget::Sidebar, Some(ModalControl::LauncherNew), "conn-new");
        modal.save_and_connect = true;
        let restored = modal.close().expect("开着才会关");
        assert_eq!(restored, (FocusTarget::Sidebar, Some(ModalControl::LauncherNew)));
        assert!(!modal.open);
        assert!(modal.trigger.is_none());
        assert!(!modal.save_and_connect);
        assert!(modal.close().is_none(), "重复关闭不再还焦一次");
    }
}
