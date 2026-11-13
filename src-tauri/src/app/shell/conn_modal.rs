//! 新建/编辑连接的全局模态生命周期。
//!
//! 草稿、机密与字段错误仍然只有一份（ConnectionPage），这里只持有“弹窗开着吗、
//! 关闭后还给谁、这一次测试属于哪一代、里面有哪些可 Tab 的控件”这类生命周期。
//! 启动台、命令面板、对象树空态和连接行操作都调用同一入口，因此不存在第二个编辑面。

use crate::app::model::FocusTarget;
use crate::app::views::connections::{engines, ConnectionPage};
use crate::connections::model::{CaMode, Engine, EnvTag, SshAuthKind, TlsMode};

use super::model::DraftField;

/// 关闭后把真实焦点还给哪个控件。
///
/// 列表行按 S12.3 由容器持有焦点、行本身不是 Tab 停靠，所以行触发时还给列表容器；
/// 静态主入口（启动台“新建”、对象树空态的“新建”）还给按钮自己。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModalControl {
    LauncherNew,
    SidebarNew,
}

// ------------------------------------------------------------ 模态里的控件 id

/// 关闭图标（头部右端）。
pub const CLOSE: &str = "conn-modal-close";
/// URL 行的“解析”按钮。
pub const APPLY_URL: &str = "conn-apply-url";
/// 文件型连接的“浏览”按钮。
pub const BROWSE_FILE: &str = "conn-browse";
pub const BROWSE_CA: &str = "conn-browse-ca";
pub const BROWSE_CERT: &str = "conn-browse-cert";
pub const BROWSE_KEY: &str = "conn-browse-key";
pub const BROWSE_SSH: &str = "conn-browse-ssh";
/// SSH 开关。
pub const SSH_TOGGLE: &str = "conn-ssh-toggle";

pub fn engine_control_id(engine: Engine) -> String {
    format!("conn-engine-{}", engine.as_str())
}

pub fn env_control_id(env: &str) -> String {
    format!("conn-env-{env}")
}

pub fn tls_control_id(mode: TlsMode) -> &'static str {
    match mode {
        TlsMode::Off => "conn-tls-off",
        TlsMode::Require => "conn-tls-require",
        TlsMode::Verify => "conn-tls-verify",
    }
}

pub fn ca_control_id(mode: CaMode) -> &'static str {
    match mode {
        CaMode::System => "conn-ca-system",
        CaMode::File => "conn-ca-file",
    }
}

pub fn ssh_auth_control_id(kind: SshAuthKind) -> &'static str {
    match kind {
        SshAuthKind::Key => "conn-ssh-auth-key",
        SshAuthKind::Password => "conn-ssh-auth-password",
    }
}

/// 环境选项：内置四个 + 自定义标签，表单与停靠点序列读同一份。
pub fn env_ids(tags: &[EnvTag]) -> Vec<String> {
    let mut ids = vec!["dev".to_owned(), "test".to_owned(), "prod".to_owned(), "local".to_owned()];
    for tag in tags {
        if !ids.iter().any(|id| id == &tag.id) {
            ids.push(tag.id.clone());
        }
    }
    ids
}

/// 模态内部的一个 Tab 停靠点。
///
/// 字段用输入框自己的焦点句柄；Control 是这一帧真实画出来的可交互元素（引擎徽标、
/// 环境/TLS/CA 分段、SSH 开关、解析/浏览按钮、关闭图标）；页脚四个动作各有一个句柄。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModalStop {
    Field(DraftField),
    Control(String),
    Cancel,
    Test,
    Save,
    SaveConnect,
}

/// 模态里一个可交互控件的动作。渲染登记、鼠标点击和回车激活读同一份登记，
/// 因此“画出来的控件”和“键盘能到的控件”不会各写一套。
#[derive(Clone, Debug, PartialEq)]
pub enum ModalAction {
    Engine(Engine),
    Env(String),
    Tls(TlsMode),
    Ca(CaMode),
    SshAuth(SshAuthKind),
    /// 走既有命令（关闭、测试、保存、解析 URL、SSH 开关）。
    Command(&'static str),
    /// 选择文件型连接的数据文件。
    DraftFile,
    /// 浏览某个路径字段（CA / 客户端证书 / 私钥 / SSH 私钥）。
    Path(DraftField),
}

impl ModalStop {
    /// 这个停靠点在屏幕上对应的元素 id；字段由输入框自己注册，返回 None。
    pub fn control_id(&self) -> Option<&str> {
        match self {
            ModalStop::Field(_) => None,
            ModalStop::Control(id) => Some(id.as_str()),
            ModalStop::Cancel => Some("conn-cancel"),
            ModalStop::Test => Some("conn-test"),
            ModalStop::Save => Some("conn-save"),
            ModalStop::SaveConnect => Some("conn-save-connect"),
        }
    }
}

/// 模态的开合、还焦与测试代号。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionModal {
    pub open: bool,
    pub origin: FocusTarget,
    pub control: Option<ModalControl>,
    /// 触发控件 id：只用于无障碍描述与测试，焦点归还走 control/列表容器。
    pub trigger: Option<String>,
    /// 每次发起测试自增。过期结果只能收回自己那一代，不能关掉更新的“正在测试”。
    pub test_generation: u64,
    pub test_running: bool,
    /// 这次保存成功后是否续接连接（“保存并连接”）。普通保存永远为 false。
    pub save_and_connect: bool,
    /// 打开后下一帧把焦点交给首个字段。
    pub focus_first_pending: bool,
    /// 上一次关闭时该还焦的控件；视图取走后清空，保证只还焦一次。
    pub last_control: Option<ModalControl>,
    /// 视图下一帧把焦点还给来源区域。
    pub restore_origin: bool,
    /// 这一帧真正画出来的可交互 id（渲染登记）。测试拿它和 stops 对账，
    /// 保证“画出来的控件”与“Tab 能到的控件”是同一份清单。
    pub rendered: Vec<String>,
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
            last_control: None,
            restore_origin: false,
            rendered: Vec::new(),
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
        self.test_running = false;
        self.test_generation = self.test_generation.saturating_add(1);
        self.rendered.clear();
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
        self.rendered.clear();
        Some((origin, control))
    }

    /// 发起一次测试：拿到本次的代号。只有代号仍是最新的那次结果才能收回指示。
    pub fn begin_test(&mut self) -> u64 {
        self.test_generation = self.test_generation.saturating_add(1);
        self.test_running = true;
        self.test_generation
    }

    /// 结果回来（成功、失败或过期）：代号仍是最新才清“正在测试”，返回是否仍是当前。
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

    /// 渲染时登记这一帧画出来的可交互 id。
    pub fn note_rendered(&mut self, id: impl Into<String>) {
        self.rendered.push(id.into());
    }

    /// 模态内的 Tab 序列：按引擎与安全选项决定屏幕上真的存在哪些字段与控件。
    /// 顺序与 connection_modal_layer 的绘制顺序一致，供测试对账。
    pub fn stops(page: &ConnectionPage, envs: &[String]) -> Vec<ModalStop> {
        let Some(draft) = page.editor.as_ref() else {
            return Vec::new();
        };
        let mut stops = vec![ModalStop::Control(CLOSE.to_owned())];
        for engine in engines() {
            stops.push(ModalStop::Control(engine_control_id(engine)));
        }
        stops.push(ModalStop::Field(DraftField::Url));
        stops.push(ModalStop::Control(APPLY_URL.to_owned()));
        stops.push(ModalStop::Field(DraftField::Name));
        for env in envs {
            stops.push(ModalStop::Control(env_control_id(env)));
        }
        if draft.engine == Engine::Sqlite {
            stops.push(ModalStop::Field(DraftField::File));
            stops.push(ModalStop::Control(BROWSE_FILE.to_owned()));
        } else {
            stops.push(ModalStop::Field(DraftField::Host));
            stops.push(ModalStop::Field(DraftField::Port));
            stops.push(ModalStop::Field(DraftField::Username));
            stops.push(ModalStop::Field(DraftField::Password));
            stops.push(ModalStop::Field(DraftField::Database));
            if draft.engine == Engine::Mongodb {
                stops.push(ModalStop::Field(DraftField::Replica));
            }
            for mode in [TlsMode::Off, TlsMode::Require, TlsMode::Verify] {
                stops.push(ModalStop::Control(tls_control_id(mode).to_owned()));
            }
            if draft.tls_mode == TlsMode::Verify {
                for mode in [CaMode::System, CaMode::File] {
                    stops.push(ModalStop::Control(ca_control_id(mode).to_owned()));
                }
                if draft.ca_mode == CaMode::File {
                    stops.push(ModalStop::Field(DraftField::CaFile));
                    stops.push(ModalStop::Control(BROWSE_CA.to_owned()));
                }
                stops.push(ModalStop::Field(DraftField::ClientCert));
                stops.push(ModalStop::Control(BROWSE_CERT.to_owned()));
                stops.push(ModalStop::Field(DraftField::ClientKey));
                stops.push(ModalStop::Control(BROWSE_KEY.to_owned()));
            }
            stops.push(ModalStop::Control(SSH_TOGGLE.to_owned()));
            if draft.ssh_on {
                stops.push(ModalStop::Field(DraftField::SshHost));
                stops.push(ModalStop::Field(DraftField::SshPort));
                stops.push(ModalStop::Field(DraftField::SshUser));
                for kind in [SshAuthKind::Key, SshAuthKind::Password] {
                    stops.push(ModalStop::Control(ssh_auth_control_id(kind).to_owned()));
                }
                if draft.ssh_auth == SshAuthKind::Key {
                    stops.push(ModalStop::Field(DraftField::SshKey));
                    stops.push(ModalStop::Control(BROWSE_SSH.to_owned()));
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
}

/// 按需连接要续接的动作。鼠标和键盘走同一条路径，因此这里只描述“连上之后做什么”。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectAction {
    /// 展开对象树（chevron）。
    Expand,
    /// 连上后进入工作区（启动台/命令搜索里的连接行双击）。
    OpenWorkspace,
    /// 打开数据页（对象树“打开数据”、双击表）。
    OpenData,
    /// 打开结构页。
    OpenStructure,
    /// 在此对象/连接上新建查询。
    NewQuery,
    /// 连上后重放这次执行命令（执行查询 / 执行当前语句 / 执行选区）。
    Run,
}

/// 一个等待连接的续接动作。spec 是触发时的对象规格，replay 是连上后要重放的命令：
/// 都按触发那一刻记下来，避免连接期间换了焦点就打开/执行别的对象。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectIntent {
    pub connection_id: String,
    pub action: ConnectAction,
    pub spec: Option<String>,
    pub replay: Option<String>,
}

impl ConnectIntent {
    pub fn new(connection_id: impl Into<String>, action: ConnectAction, spec: Option<String>) -> Self {
        Self { connection_id: connection_id.into(), action, spec, replay: None }
    }

    /// 连上后要重放的执行命令（只有 Run 用得到）。
    pub fn with_replay(mut self, command: impl Into<String>) -> Self {
        self.replay = Some(command.into());
        self
    }

    /// 连接成功后要执行的命令；Expand 只改树状态，没有命令。
    pub fn command(&self) -> Option<&str> {
        match self.action {
            ConnectAction::Expand => None,
            ConnectAction::OpenWorkspace => Some("activity-workspace"),
            ConnectAction::OpenData => Some("tree-open-data"),
            ConnectAction::OpenStructure => Some("open-structure"),
            ConnectAction::NewQuery => Some("tree-new-query"),
            ConnectAction::Run => self.replay.as_deref(),
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

    fn controls(stops: &[ModalStop]) -> Vec<String> {
        stops
            .iter()
            .filter_map(|stop| match stop {
                ModalStop::Control(id) => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    /// 控件停靠点覆盖“能改变配置的那几组”：引擎、环境、TLS/CA、SSH、解析与浏览、关闭。
    #[test]
    fn stops_include_every_interactive_group_not_just_fields() {
        let mut page = page(Engine::Postgres);
        page.editor.as_mut().unwrap().tls_mode = TlsMode::Verify;
        page.editor.as_mut().unwrap().ca_mode = CaMode::File;
        page.editor.as_mut().unwrap().ssh_on = true;
        let ids = controls(&ConnectionModal::stops(&page, &["dev".to_owned(), "prod".to_owned()]));
        for expected in [
            CLOSE,
            APPLY_URL,
            "conn-engine-sqlite",
            "conn-engine-postgres",
            "conn-env-dev",
            "conn-env-prod",
            "conn-tls-verify",
            "conn-ca-file",
            BROWSE_CA,
            BROWSE_CERT,
            BROWSE_KEY,
            SSH_TOGGLE,
            "conn-ssh-auth-key",
            BROWSE_SSH,
        ] {
            assert!(ids.iter().any(|id| id == expected), "缺少停靠点 {expected}: {ids:?}");
        }
        assert!(!ids.iter().any(|id| id == BROWSE_FILE), "非文件型连接没有文件浏览按钮");
    }

    #[test]
    fn stops_follow_the_shape_of_the_form() {
        let sqlite = ConnectionModal::stops(&page(Engine::Sqlite), &[]);
        assert!(sqlite.iter().all(|stop| *stop != ModalStop::Field(DraftField::Host)), "文件型没有 host");
        assert!(sqlite.contains(&ModalStop::Field(DraftField::File)));
        assert!(!controls(&sqlite).iter().any(|id| id == SSH_TOGGLE), "文件型没有 SSH");

        let mut remote = page(Engine::Postgres);
        remote.editor.as_mut().unwrap().ssh_on = true;
        remote.editor.as_mut().unwrap().ssh_auth = SshAuthKind::Password;
        let stops = ConnectionModal::stops(&remote, &[]);
        assert!(stops.contains(&ModalStop::Field(DraftField::SshPassword)));
        assert!(!stops.contains(&ModalStop::Field(DraftField::SshKey)), "密码认证没有私钥字段");
        assert!(!controls(&stops).iter().any(|id| id == BROWSE_SSH), "密码认证没有私钥浏览");

        remote.editor.as_mut().unwrap().tls_mode = TlsMode::Verify;
        remote.editor.as_mut().unwrap().ca_mode = CaMode::File;
        remote.editor.as_mut().unwrap().ssh_on = false;
        assert!(ConnectionModal::stops(&remote, &[]).contains(&ModalStop::Field(DraftField::CaFile)));
    }

    /// 首尾相接靠的是 GPUI 的 focus trap（Root 的 Tab 动作在容器内成环）；
    /// 这里只保证首末项固定，真实 Tab 由 shell 测试验证。
    #[test]
    fn the_sequence_starts_at_close_and_ends_at_save_and_connect() {
        let stops = ConnectionModal::stops(&page(Engine::Postgres), &["dev".to_owned()]);
        assert_eq!(stops.first(), Some(&ModalStop::Control(CLOSE.to_owned())));
        assert_eq!(stops.last(), Some(&ModalStop::SaveConnect));
        assert!(stops.len() > 10, "字段+控件+页脚都要在序列里：{stops:?}");
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
        modal.note_rendered(CLOSE);
        let restored = modal.close().expect("开着才会关");
        assert_eq!(restored, (FocusTarget::Sidebar, Some(ModalControl::LauncherNew)));
        assert!(!modal.open);
        assert!(modal.trigger.is_none());
        assert!(!modal.save_and_connect);
        assert!(modal.rendered.is_empty(), "关闭后不再留上一帧的登记");
        assert!(modal.close().is_none(), "重复关闭不再还焦一次");
    }
}
