//! 对象树的危险操作确认层：删除表/视图、清空表、重命名表。
//!
//! 和结构页同一套规则：先按 `ddl` 生成计划与风险，预览里看到要执行的语句；
//! 第一次点“执行”只是待确认，第二次才执行。连接的环境标签 tone 为 `prod` 时，
//! 还必须输入与对象名完全一致的文本，执行才可用。取消或关闭不发送任何语句。
//! 这里只有纯状态；读取计划与执行由视图在已打开的会话上发起。

use crate::catalog::hierarchy::ObjectKey;
use crate::catalog::model::{Engine, ObjectKind};
use crate::connections::model::{EnvTag, ENV_NONE};
use crate::ddl::{Action, Plan, Receipt, StepStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DangerKind {
    DropTable,
    DropView,
    Truncate,
    Rename,
}

impl DangerKind {
    /// 菜单项 → 操作。种类不符（对视图清空/重命名）时返回 None。
    pub fn from_menu(id: &str, kind: ObjectKind) -> Option<Self> {
        match (id, kind) {
            ("tree-drop-object", ObjectKind::Table) => Some(Self::DropTable),
            ("tree-drop-object", ObjectKind::View) => Some(Self::DropView),
            ("tree-truncate-table", ObjectKind::Table) => Some(Self::Truncate),
            ("tree-rename-table", ObjectKind::Table) => Some(Self::Rename),
            _ => None,
        }
    }

    pub fn title_key(self) -> &'static str {
        match self {
            Self::DropTable => "catalog.tree.dropTable",
            Self::DropView => "catalog.tree.dropView",
            Self::Truncate => "catalog.tree.truncateTable",
            Self::Rename => "catalog.tree.renameTable",
        }
    }
}

/// 连接的环境是不是生产：按环境标签表里这条标签的 tone 判断，默认标签和自建标签一样。
/// 标签表读不到时，只要连接挂着标签就按生产处理（宁可多输一次名字）。
pub fn env_is_prod(env: &str, tags: Option<&[EnvTag]>) -> bool {
    let env = env.trim();
    if env.is_empty() || env == ENV_NONE {
        return false;
    }
    match tags {
        Some(tags) => tags.iter().find(|tag| tag.id == env).is_some_and(|tag| tag.tone == "prod"),
        None => true,
    }
}

/// 预览里要展示的计划。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DangerPlan {
    pub sql: Vec<String>,
    /// (level, text)，level 为 danger / warn / info。
    pub risks: Vec<(String, String)>,
    pub blocked: Vec<String>,
    pub rule: String,
}

impl DangerPlan {
    pub fn of(plan: &Plan) -> Self {
        Self {
            sql: plan.steps.iter().map(|step| step.sql.clone()).collect(),
            risks: plan.risks.iter().map(|risk| (risk.level.clone(), risk.text.clone())).collect(),
            blocked: plan.blocked.clone(),
            rule: plan.rule.clone(),
        }
    }
}

/// 执行结果：被拦下或任何一条语句没有成功都算失败，带上数据库给的原因。
/// 原因逐条给出：英文界面先逐条换成英文再拼起来（拼好的整句不在句型表里）。
pub fn receipt_problems(receipt: &Receipt) -> Result<(), Vec<String>> {
    if !receipt.blocked.is_empty() {
        return Err(receipt.blocked.clone());
    }
    let failed: Vec<String> = receipt
        .steps
        .iter()
        .filter(|step| step.status != StepStatus::Ok)
        .map(|step| if step.message.is_empty() { step.sql.clone() } else { step.message.clone() })
        .collect();
    if receipt.steps.is_empty() {
        return Err(vec!["没有执行任何语句".to_owned()]);
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed)
    }
}

/// 视图要替模型做的一步异步工作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DangerStep {
    Preview,
    Execute,
}

#[derive(Clone, Debug)]
pub struct DangerPrompt {
    /// 每次打开一个新编号；迟到的预览/执行结果按编号丢弃。
    pub epoch: u64,
    pub kind: DangerKind,
    pub node_id: String,
    pub object: ObjectKey,
    /// 连接上保存的环境标签 id。
    pub env: String,
    /// 展示用的标签名；标签表到之前先用 id。
    pub env_label: String,
    pub prod: bool,
    /// 生产环境要求输入的对象名。
    pub typed: String,
    /// 重命名的新名字。
    pub new_name: String,
    pub plan: Option<DangerPlan>,
    pub loading: bool,
    pub armed: bool,
    pub running: bool,
    pub error: Option<String>,
    /// `error` 拆开的各条原因（按界面语言逐条显示）。
    error_parts: Vec<String>,
    request: Option<DangerStep>,
}

impl DangerPrompt {
    pub fn new(kind: DangerKind, node_id: &str, object: ObjectKey, env: &str) -> Self {
        let mut prompt = Self {
            epoch: super::model::next_epoch(),
            kind,
            node_id: node_id.to_owned(),
            object,
            env: env.to_owned(),
            env_label: env.to_owned(),
            prod: env_is_prod(env, None),
            typed: String::new(),
            new_name: String::new(),
            plan: None,
            loading: false,
            armed: false,
            running: false,
            error: None,
            error_parts: Vec::new(),
            request: None,
        };
        // 重命名要先有新名字才生成计划；其余打开即生成预览。
        if kind != DangerKind::Rename {
            prompt.request_preview();
        }
        prompt
    }

    pub fn engine(&self) -> Engine {
        self.object.engine
    }

    pub fn action(&self) -> Action {
        let name = self.object.name.clone();
        match self.kind {
            DangerKind::DropTable => Action::DropTable { table: name },
            DangerKind::DropView => Action::DropView { view: name },
            DangerKind::Truncate => Action::TruncateTable { table: name },
            DangerKind::Rename => Action::RenameTable { table: name, new_name: self.new_name.trim().to_owned() },
        }
    }

    fn set_error(&mut self, parts: Vec<String>) {
        self.error = Some(parts.join(" "));
        self.error_parts = parts;
    }

    fn clear_error(&mut self) {
        self.error = None;
        self.error_parts.clear();
    }

    /// 确认层显示的错误：英文界面逐条换成英文后再拼接；数据库原文不改。
    pub fn error_text(&self, locale: crate::i18n::Locale) -> Option<String> {
        self.error.as_ref()?;
        Some(self.error_parts.iter().map(|part| crate::ddl::english::localize(part, locale)).collect::<Vec<_>>().join(" "))
    }

    pub fn take_request(&mut self) -> Option<DangerStep> {
        self.request.take()
    }

    pub fn request_preview(&mut self) {
        self.plan = None;
        self.armed = false;
        self.clear_error();
        self.loading = true;
        self.request = Some(DangerStep::Preview);
    }

    /// 标签表到了：按 tone 重新判断是否生产。
    pub fn apply_tags(&mut self, tags: Option<&[EnvTag]>) {
        self.prod = env_is_prod(&self.env, tags);
        if let Some(tag) = tags.and_then(|tags| tags.iter().find(|tag| tag.id == self.env)) {
            self.env_label = tag.label.clone();
        }
    }

    pub fn apply_plan(&mut self, result: Result<Plan, String>) {
        self.loading = false;
        self.armed = false;
        match result {
            Ok(plan) => {
                self.plan = Some(DangerPlan::of(&plan));
                self.clear_error();
            }
            Err(message) => {
                self.plan = None;
                self.set_error(vec![message]);
            }
        }
    }

    pub fn set_typed(&mut self, text: String) {
        self.typed = text;
    }

    /// 新名字一改，旧计划就作废：必须重新预览才能执行。
    pub fn set_new_name(&mut self, text: String) {
        if self.new_name == text {
            return;
        }
        self.new_name = text;
        self.plan = None;
        self.armed = false;
        self.clear_error();
    }

    pub fn typed_matches(&self) -> bool {
        self.typed == self.object.name
    }

    /// 执行按钮是否可点：有可执行的计划、不在读取或执行中，生产环境还要输对对象名。
    pub fn execute_ready(&self) -> bool {
        let plan_ok = self.plan.as_ref().is_some_and(|plan| plan.blocked.is_empty() && !plan.sql.is_empty());
        plan_ok && !self.loading && !self.running && (!self.prod || self.typed_matches())
    }

    /// 点击“执行”：第一次只进入待确认，第二次才排队执行。返回是否真的排队了执行。
    pub fn click_execute(&mut self) -> bool {
        if !self.execute_ready() {
            return false;
        }
        if !self.armed {
            self.armed = true;
            return false;
        }
        self.armed = false;
        self.running = true;
        self.clear_error();
        self.request = Some(DangerStep::Execute);
        true
    }

    /// 执行结束。成功返回 true；失败把数据库原因留在确认层里，计划保留可重试。
    pub fn apply_receipt(&mut self, result: Result<Receipt, String>) -> bool {
        self.running = false;
        self.armed = false;
        match result.map_err(|message| vec![message]).and_then(|receipt| receipt_problems(&receipt)) {
            Ok(()) => true,
            Err(parts) => {
                self.set_error(parts);
                false
            }
        }
    }

    pub fn confirmed_sql(&self) -> Vec<String> {
        self.plan.as_ref().map(|plan| plan.sql.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl::{DdlEngine, Snapshot};

    fn tag(id: &str, tone: &str) -> EnvTag {
        EnvTag { id: id.to_owned(), label: id.to_owned(), tone: tone.to_owned(), position: 0 }
    }

    fn key(engine: Engine, name: &str, kind: ObjectKind) -> ObjectKey {
        ObjectKey {
            connection_id: "c1".to_owned(),
            connection_name: "库".to_owned(),
            engine,
            database: "main".to_owned(),
            schema: String::new(),
            name: name.to_owned(),
            kind,
        }
    }

    fn sqlite_plan(prompt: &DangerPrompt) -> Plan {
        let mut snapshot = Snapshot::bare(DdlEngine::Sqlite, "main", "", &prompt.object.name);
        snapshot.kind = "table".to_owned();
        snapshot.name_taken = Some(false);
        crate::ddl::preview_plan(&snapshot, &prompt.action())
    }

    #[test]
    fn prod_follows_the_tag_tone_including_custom_tags() {
        let tags = vec![tag("dev", "dev"), tag("prod", "prod"), tag("live", "prod"), tag("staging", "test")];
        assert!(env_is_prod("prod", Some(&tags)));
        assert!(env_is_prod("live", Some(&tags)), "自建标签按 tone 判断");
        assert!(!env_is_prod("dev", Some(&tags)));
        assert!(!env_is_prod("staging", Some(&tags)));
        assert!(!env_is_prod("none", Some(&tags)));
        assert!(!env_is_prod("", None));
        // 标签被删掉或改了 tone：不再按名字猜。
        let renamed = vec![tag("prod", "test")];
        assert!(!env_is_prod("prod", Some(&renamed)));
        // 标签表读不到：挂着标签就按生产处理。
        assert!(env_is_prod("dev", None));
    }

    #[test]
    fn execute_arms_first_then_runs_and_prod_needs_the_exact_name() {
        let mut prompt = DangerPrompt::new(DangerKind::DropTable, "n", key(Engine::Sqlite, "orders", ObjectKind::Table), "prod");
        assert_eq!(prompt.take_request(), Some(DangerStep::Preview), "打开即生成预览");
        assert!(prompt.take_request().is_none());
        assert!(!prompt.execute_ready(), "还没有计划");
        let plan = sqlite_plan(&prompt);
        prompt.apply_plan(Ok(plan));
        prompt.apply_tags(Some(&[tag("prod", "prod")]));
        assert!(prompt.prod);
        let sql = prompt.confirmed_sql();
        assert!(sql.iter().any(|line| line.contains("DROP TABLE")), "{sql:?}");
        assert!(prompt.plan.as_ref().unwrap().risks.iter().any(|(level, _)| level == "danger"));

        // 生产：名字不对，执行一直不可用。
        assert!(!prompt.execute_ready());
        assert!(!prompt.click_execute());
        assert!(!prompt.armed);
        prompt.set_typed("Orders".to_owned());
        assert!(!prompt.execute_ready(), "大小写不同也不算");
        prompt.set_typed("orders ".to_owned());
        assert!(!prompt.execute_ready(), "多一个空格也不算");
        prompt.set_typed("orders".to_owned());
        assert!(prompt.execute_ready());

        // 第一次只待确认，第二次才排队执行。
        assert!(!prompt.click_execute());
        assert!(prompt.armed);
        assert!(prompt.take_request().is_none(), "第一次点击不发送任何语句");
        assert!(prompt.click_execute());
        assert_eq!(prompt.take_request(), Some(DangerStep::Execute));
        assert!(prompt.running);
        assert!(!prompt.execute_ready(), "执行中不能再点");

        // 失败：原因留在确认层，回到未待确认状态。
        assert!(!prompt.apply_receipt(Err("no such table: orders".to_owned())));
        assert_eq!(prompt.error.as_deref(), Some("no such table: orders"));
        assert!(!prompt.armed && !prompt.running);
    }

    /// 多条拦下原因先逐条换成英文再拼接（拼好的整句不在句型表里）；数据库原文原样；中文界面是原文。
    #[test]
    fn receipt_errors_translate_each_item_before_joining() {
        use crate::i18n::Locale as L;
        let receipt = |blocked: &[&str]| Receipt {
            blocked: blocked.iter().map(|text| (*text).to_owned()).collect(),
            rule: String::new(),
            steps: Vec::new(),
            target: String::new(),
            rebuild: false,
        };
        let mut prompt = DangerPrompt::new(DangerKind::Rename, "n", key(Engine::Sqlite, "orders", ObjectKind::Table), "");
        assert!(!prompt.apply_receipt(Ok(receipt(&["新表名不能为空。", "名字 a、b 已经被其它对象占用，不会覆盖它。"]))));
        assert_eq!(prompt.error.as_deref(), Some("新表名不能为空。 名字 a、b 已经被其它对象占用，不会覆盖它。"));
        assert_eq!(
            prompt.error_text(L::En).as_deref(),
            Some("The new table name can't be empty. The name a、b is already used by another object; it won't be overwritten.")
        );
        assert_eq!(prompt.error_text(L::ZhCn), prompt.error);
        assert!(!prompt.apply_receipt(Ok(receipt(&[]))));
        assert_eq!(prompt.error_text(L::En).as_deref(), Some("No statement was run"));
        for fallback in ["生成计划失败", "执行失败"] {
            assert!(!prompt.apply_receipt(Err(fallback.to_owned())));
            let shown = prompt.error_text(L::En).unwrap();
            assert!(!crate::ddl::english::has_cjk(&shown), "{shown}");
        }
        prompt.apply_plan(Err("no such table: orders".to_owned()));
        assert_eq!(prompt.error_text(L::En).as_deref(), Some("no such table: orders"));
        prompt.request_preview();
        assert!(prompt.error.is_none() && prompt.error_text(L::En).is_none());
    }

    #[test]
    fn non_prod_and_rename_rules() {
        let mut dev = DangerPrompt::new(DangerKind::Truncate, "n", key(Engine::Sqlite, "orders", ObjectKind::Table), "dev");
        dev.apply_tags(Some(&[tag("dev", "dev")]));
        let plan = sqlite_plan(&dev);
        dev.apply_plan(Ok(plan));
        assert!(!dev.prod);
        assert!(dev.execute_ready(), "非生产不需要输入名字");
        assert!(dev.confirmed_sql().iter().any(|line| line.starts_with("DELETE FROM")), "{:?}", dev.confirmed_sql());

        let mut rename = DangerPrompt::new(DangerKind::Rename, "n", key(Engine::Sqlite, "orders", ObjectKind::Table), "none");
        assert!(rename.take_request().is_none(), "重命名先等新名字");
        let plan = sqlite_plan(&rename);
        rename.apply_plan(Ok(plan));
        assert!(!rename.execute_ready(), "空名字被计划拦下");
        assert!(!rename.plan.as_ref().unwrap().blocked.is_empty());
        rename.set_new_name("orders_old".to_owned());
        assert!(rename.plan.is_none(), "改了名字旧计划作废");
        rename.request_preview();
        assert_eq!(rename.take_request(), Some(DangerStep::Preview));
        let plan = sqlite_plan(&rename);
        rename.apply_plan(Ok(plan));
        assert!(rename.execute_ready());
        assert!(rename.confirmed_sql().iter().any(|line| line.contains("RENAME TO")), "{:?}", rename.confirmed_sql());

        assert_eq!(DangerKind::from_menu("tree-drop-object", ObjectKind::View), Some(DangerKind::DropView));
        assert_eq!(DangerKind::from_menu("tree-truncate-table", ObjectKind::View), None);
        assert_eq!(DangerKind::from_menu("tree-rename-table", ObjectKind::Sequence), None);
    }
}
