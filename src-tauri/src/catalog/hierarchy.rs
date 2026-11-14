//! 可展开的目录树模型：真实库/模式/对象类型层级、按需子节点、局部失败与续读。
//!
//! 这一层不依赖 GPUI，也不自己发查询：外壳把 catalog 读取结果喂进 [`NavTree`]，
//! 渲染、搜索高亮、键盘移动和对象动作都读同一份节点身份。节点 id 只由
//! 连接/库/模式/种类/名称组成，显示名不参与身份。

use std::collections::{BTreeMap, BTreeSet};

use crate::catalog::model::{
    kind_name, listed_kinds, ConnStatus, DatabaseNode, Engine, KindReport, ObjectKind, ObjectRow, SchemaNode,
};

/// 节点种类。列和索引是对象的按需子节点，不参与对象种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NavKind {
    Connection,
    Database,
    Schema,
    /// 引擎真实拥有的对象类型分组；没有对应层级的引擎不产生这一层。
    Group,
    Object,
    ColumnGroup,
    Column,
    IndexGroup,
    Index,
    /// Redis 键空间（SELECT 的库号）；SCAN 游标挂在它上面。
    Keyspace,
}

impl NavKind {
    pub fn is_object(self) -> bool {
        matches!(self, NavKind::Object)
    }
}

fn join(parts: &[&str]) -> String {
    parts.iter().filter(|part| !part.is_empty()).copied().collect::<Vec<_>>().join("\u{1}")
}

pub fn connection_node_id(connection_id: &str) -> String {
    join(&["conn", connection_id])
}

pub fn database_node_id(connection_id: &str, database: &str) -> String {
    join(&["db", connection_id, database])
}

pub fn keyspace_node_id(connection_id: &str, keyspace: &str) -> String {
    join(&["space", connection_id, keyspace])
}

pub fn schema_node_id(connection_id: &str, database: &str, schema: &str) -> String {
    join(&["schema", connection_id, database, schema])
}

pub fn group_node_id(connection_id: &str, database: &str, schema: &str, kind: ObjectKind) -> String {
    join(&["group", connection_id, database, schema, kind_name(kind)])
}

pub fn object_node_id(connection_id: &str, database: &str, schema: &str, object: &ObjectRow) -> String {
    join(&[
        "obj",
        connection_id,
        database,
        schema,
        kind_name(object.kind),
        &object.name,
        &object.signature,
        &object.relation,
    ])
}

pub fn column_group_id(object_id: &str) -> String {
    join(&["cols", object_id])
}

pub fn column_node_id(object_id: &str, column: &str) -> String {
    join(&["col", object_id, column])
}

pub fn index_group_id(object_id: &str) -> String {
    join(&["idxs", object_id])
}

pub fn index_node_id(object_id: &str, index: &str) -> String {
    join(&["idx", object_id, index])
}

/// 对象动作要的身份。展示名、环境不参与，两个连接或同连接同名对象不会串目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectKey {
    pub connection_id: String,
    pub connection_name: String,
    pub engine: Engine,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
}

impl ObjectKey {
    /// 复制用的限定名：按引擎的层级写，不把展示用的连接名带进去。
    pub fn qualified_name(&self) -> String {
        match self.engine {
            Engine::Sqlite => self.name.clone(),
            Engine::Mysql | Engine::Mariadb => {
                if self.database.is_empty() {
                    self.name.clone()
                } else {
                    format!("{}.{}", self.database, self.name)
                }
            }
            _ => {
                if self.schema.is_empty() {
                    self.name.clone()
                } else {
                    format!("{}.{}", self.schema, self.name)
                }
            }
        }
    }

    /// 该对象能否打开数据页：只有表和视图有行数据，例程/触发器/序列/类型都不行。
    pub fn opens_data(self: &ObjectKey) -> bool {
        matches!(self.kind, ObjectKind::Table | ObjectKind::View)
    }

    pub fn kind_label_key(&self) -> &'static str {
        match self.kind {
            ObjectKind::Table => "catalog.kind.table",
            ObjectKind::View => "catalog.kind.view",
            ObjectKind::Function => "catalog.kind.function",
            ObjectKind::Procedure => "catalog.kind.procedure",
            ObjectKind::Trigger => "catalog.kind.trigger",
            ObjectKind::Sequence => "catalog.kind.sequence",
            ObjectKind::Enum => "catalog.kind.enum",
            ObjectKind::Domain => "catalog.kind.domain",
            ObjectKind::Extension => "catalog.kind.extension",
            ObjectKind::Event => "catalog.kind.event",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavNode {
    pub id: String,
    pub parent: Option<String>,
    pub kind: NavKind,
    pub label: String,
    /// 分组节点用文案键；对象和列用真实名字。
    pub label_key: Option<&'static str>,
    /// 完整路径：连接 / 库 / 模式 / 对象。搜索命中后整段显示。
    pub path: String,
    pub connection_id: String,
    pub connection_name: String,
    pub engine: Engine,
    pub database: String,
    pub schema: String,
    pub name: String,
    pub object_kind: Option<ObjectKind>,
    pub expandable: bool,
    /// 已知对象总量；0 表示未知。
    pub total: usize,
    /// 已经读进来的对象数量。
    pub loaded: usize,
    pub has_more: bool,
    /// Redis SCAN 游标。
    pub cursor: Option<String>,
    pub error: Option<String>,
    pub loading: bool,
    /// 对象种类尚未读取；展开时只取这个分组的第一页。
    pub unloaded: bool,
    pub paged: bool,
    pub stale: bool,
    pub children: Vec<String>,
    /// PostgreSQL 重载函数的参数签名；读定义时用来定位同名对象。其余为空。
    pub signature: String,
    /// 触发器所属的表；读定义时用。其余为空。
    pub relation: String,
    /// 列节点是否属于主键；SQL 模板据此写 WHERE。其余为 false。
    pub primary: bool,
}

impl NavNode {
    fn new(kind: NavKind, id: String, label: String) -> Self {
        Self {
            id,
            parent: None,
            kind,
            label,
            label_key: None,
            path: String::new(),
            connection_id: String::new(),
            connection_name: String::new(),
            engine: Engine::Sqlite,
            database: String::new(),
            schema: String::new(),
            name: String::new(),
            object_kind: None,
            expandable: false,
            total: 0,
            loaded: 0,
            has_more: false,
            cursor: None,
            error: None,
            loading: false,
            unloaded: false,
            paged: false,
            stale: false,
            children: Vec::new(),
            signature: String::new(),
            relation: String::new(),
            primary: false,
        }
    }

    pub fn object(&self) -> Option<ObjectKey> {
        self.object_kind.map(|kind| ObjectKey {
            connection_id: self.connection_id.clone(),
            connection_name: self.connection_name.clone(),
            engine: self.engine,
            database: self.database.clone(),
            schema: self.schema.clone(),
            name: self.name.clone(),
            kind,
        })
    }
}

/// 一次 catalog 读取里的一条连接。库/模式/对象的局部失败都留在自己那一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavConnection {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    pub status: ConnStatus,
    pub error: Option<String>,
    /// 连接默认所在的库（Redis 就是 SELECT 的库号），没有时为空。
    pub default_database: Option<String>,
    pub outline_only: bool,
    pub databases: Vec<DatabaseNode>,
}

impl NavConnection {
    pub fn new(id: impl Into<String>, name: impl Into<String>, engine: Engine, status: ConnStatus) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            engine,
            status,
            error: None,
            default_database: None,
            outline_only: false,
            databases: Vec::new(),
        }
    }
}

/// 可见行：深度、展开状态、焦点和搜索高亮都按同一份节点算。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavRow {
    pub id: String,
    pub depth: usize,
    pub kind: NavKind,
    pub label: String,
    /// 节点的真实名称（对象名、库名、列名），显示名可能在前面加上模式。
    pub name: String,
    pub label_key: Option<&'static str>,
    pub path: String,
    pub expanded: bool,
    pub focused: bool,
    pub expandable: bool,
    /// 对象行与对象种类分组携带种类；列/索引/库/模式为空。
    pub object_kind: Option<ObjectKind>,
    pub stale: bool,
    pub loading: bool,
    pub unloaded: bool,
    pub error: Option<String>,
    pub has_more: bool,
    pub total: usize,
    pub loaded: usize,
    /// 高亮区间（字节偏移），只在搜索命中时给出。
    pub highlight: Option<(usize, usize)>,
    /// 列：类型与约束（如 "INTEGER · pk · not null"）；索引：所含列与唯一性。其余为空。
    pub detail: String,
    /// 列是否属于主键。
    pub primary: bool,
}

#[derive(Debug, Clone)]
pub struct NavTree {
    nodes: BTreeMap<String, NavNode>,
    roots: Vec<String>,
    expanded: BTreeSet<String>,
    focus: Option<String>,
    query: String,
    saved_expansion: Option<BTreeSet<String>>,
    saved_focus: Option<String>,
}

/// 键盘移动后要执行的动作。树自己处理展开状态，其余交给外壳。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavAction {
    None,
    /// 向上级节点移动焦点（左方向键在已折叠节点上）。
    FocusParent,
    /// 展开并取子节点（右方向键/续读）。
    Load(String),
    /// 打开数据页；只有表和视图会返回它。
    OpenData(String),
    /// 打开结构/定义页。
    OpenStructure(String),
    /// 打开 Redis 键空间。
    OpenKeyspace { connection_id: String, keyspace: String },
    /// 打开 MongoDB 集合的文档页。
    OpenDocuments(String),
    /// 把列名插入当前草稿。
    InsertColumn { object_id: String, column: String },
}

impl Default for NavTree {
    fn default() -> Self {
        Self::new()
    }
}

impl NavTree {
    pub fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
            roots: Vec::new(),
            expanded: BTreeSet::new(),
            focus: None,
            query: String::new(),
            saved_expansion: None,
            saved_focus: None,
        }
    }

    pub fn node(&self, id: &str) -> Option<&NavNode> {
        self.nodes.get(id)
    }

    pub fn nodes(&self) -> impl Iterator<Item = &NavNode> {
        self.nodes.values()
    }

    pub fn roots(&self) -> &[String] {
        &self.roots
    }

    pub fn focus(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    pub fn focus_node(&self) -> Option<&NavNode> {
        self.focus.as_deref().and_then(|id| self.nodes.get(id))
    }

    pub fn empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// 整个重建，同时保留展开、焦点，以及对象已有的列/索引子节点。
    /// 返回 true 表示可见内容确实变了。
    pub fn apply(&mut self, connections: &[NavConnection]) -> bool {
        let changed = connections.iter().map(|connection| connection.id.clone()).collect();
        self.apply_changed(connections, &changed)
    }

    pub fn apply_changed(&mut self, connections: &[NavConnection], changed: &BTreeSet<String>) -> bool {
        let previous = std::mem::take(&mut self.nodes);
        let previous_roots = std::mem::take(&mut self.roots);
        let previous_ids: Vec<String> = previous.values().map(|node| node.id.clone()).collect();
        for connection in connections {
            let root_id = connection_node_id(&connection.id);
            self.roots.push(root_id.clone());
            if !changed.contains(&connection.id) && previous.contains_key(&root_id) {
                let mut pending = vec![root_id];
                while let Some(id) = pending.pop() {
                    if let Some(node) = previous.get(&id) {
                        pending.extend(node.children.iter().cloned());
                        self.nodes.insert(id, node.clone());
                    }
                }
            } else {
                self.push_connection(connection, &root_id, &previous);
            }
        }
        self.expanded.retain(|id| self.nodes.contains_key(id));
        if self.focus.as_deref().is_some_and(|id| !self.nodes.contains_key(id)) {
            self.focus = self
                .visible()
                .first()
                .map(|row| row.id.clone())
                .or_else(|| self.roots.first().cloned());
        }
        let changed = previous_ids != self.nodes.keys().cloned().collect::<Vec<_>>()
            || previous_roots != self.roots;
        changed
    }

    fn push_connection(&mut self, connection: &NavConnection, root_id: &str, previous: &BTreeMap<String, NavNode>) {
        let mut root = NavNode::new(NavKind::Connection, root_id.to_owned(), connection.name.clone());
        root.path = connection.name.clone();
        root.connection_id = connection.id.clone();
        root.connection_name = connection.name.clone();
        root.engine = connection.engine;
        root.name = connection.name.clone();
        root.error = connection.error.clone();
        root.expandable = true;
        self.nodes.insert(root_id.to_owned(), root);

        if connection.status != ConnStatus::Connected {
            return;
        }
        match connection.engine {
            // Redis 没有库/schema 层级：键空间就是 SELECT 的库号。
            Engine::Redis => self.push_keyspaces(connection, root_id),
            _ => self.push_databases(connection, root_id, previous),
        }
    }

    fn push_keyspaces(&mut self, connection: &NavConnection, root_id: &str) {
        // 只画会话实际所在的键空间，不凭空列出 16 个库；SCAN 游标挂在这个节点上。
        let keyspace = connection
            .default_database
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "0".to_owned());
        let id = keyspace_node_id(&connection.id, &keyspace);
        let mut node = NavNode::new(NavKind::Keyspace, id, format!("db{keyspace}"));
        node.parent = Some(root_id.to_owned());
        node.path = connection.name.clone();
        node.connection_id = connection.id.clone();
        node.connection_name = connection.name.clone();
        node.engine = connection.engine;
        node.database = keyspace.clone();
        node.name = keyspace;
        node.expandable = true;
        node.total = 0;
        self.attach(root_id, node);
    }

    fn push_databases(&mut self, connection: &NavConnection, root_id: &str, previous: &BTreeMap<String, NavNode>) {
        // 没有库层级的引擎（SQLite）直接把对象挂到连接下；不制造空库标题。
        if !connection.engine.shows_databases() {
            let schema = connection
                .databases
                .first()
                .and_then(|database| database.schemas.first())
                .cloned()
                .unwrap_or_else(|| SchemaNode::empty(""));
            let database_name = connection.databases.first().map(|database| database.name.clone()).unwrap_or_default();
            self.push_schema_objects(connection, root_id, &database_name, &schema, previous);
            return;
        }
        for database in &connection.databases {
            let db_id = database_node_id(&connection.id, &database.name);
            let mut node = NavNode::new(NavKind::Database, db_id.clone(), database.name.clone());
            node.parent = Some(root_id.to_owned());
            node.path = format!("{} / {}", connection.name, database.name);
            node.connection_id = connection.id.clone();
            node.connection_name = connection.name.clone();
            node.engine = connection.engine;
            node.database = database.name.clone();
            node.name = database.name.clone();
            node.error = database.error.clone();
            node.expandable = database.error.is_none();
            // 会话当前库以外的库只带回来一个库名：模式与对象等展开这一行时按库读一次。
            node.unloaded = node.expandable && connection.engine.shows_schemas() && database.schemas.is_empty();
            self.attach(root_id, node);
            if database.error.is_some() {
                continue;
            }
            if connection.engine == Engine::Mongodb {
                // MongoDB 没有对象种类层：库下面直接是集合，集合打开文档页。
                let schema = database
                    .schemas
                    .iter()
                    .find(|schema| schema.name.is_empty())
                    .or_else(|| database.schemas.first());
                if let Some(schema) = schema {
                    for object in &schema.objects {
                        let object_id = object_node_id(&connection.id, &database.name, &schema.name, object);
                        let mut node = self.object_node(connection, &database.name, &schema.name, object, &object_id);
                        node.expandable = false;
                        self.attach(&db_id, node);
                    }
                    if let Some(parent) = self.nodes.get_mut(&db_id) {
                        parent.total = schema.total.max(schema.objects.len());
                        parent.loaded = schema.objects.len();
                    }
                }
            } else if connection.engine.shows_schemas() {
                for schema in &database.schemas {
                    self.push_schema(connection, &db_id, &database.name, schema, previous);
                }
            } else {
                // MySQL/MariaDB 没有 schema 层：库下面直接是对象种类。
                let schema = database
                    .schemas
                    .iter()
                    .find(|schema| schema.name.is_empty())
                    .or_else(|| database.schemas.first());
                if let Some(schema) = schema {
                    self.push_schema_objects(connection, &db_id, &database.name, schema, previous);
                }
            }
        }
    }

    fn push_schema(
        &mut self,
        connection: &NavConnection,
        parent_id: &str,
        database: &str,
        schema: &SchemaNode,
        previous: &BTreeMap<String, NavNode>,
    ) {
        let id = schema_node_id(&connection.id, database, &schema.name);
        let mut node = NavNode::new(NavKind::Schema, id.clone(), schema.name.clone());
        node.parent = Some(parent_id.to_owned());
        node.path = format!("{} / {} / {}", connection.name, database, schema.name);
        node.connection_id = connection.id.clone();
        node.connection_name = connection.name.clone();
        node.engine = connection.engine;
        node.database = database.to_owned();
        node.schema = schema.name.clone();
        node.name = schema.name.clone();
        node.error = schema.error.clone();
        node.expandable = schema.error.is_none();
        self.attach(parent_id, node);
        if schema.error.is_some() {
            return;
        }
        self.push_schema_objects(connection, &id, database, schema, previous);
    }

    fn push_schema_objects(
        &mut self,
        connection: &NavConnection,
        parent_id: &str,
        database: &str,
        schema: &SchemaNode,
        previous: &BTreeMap<String, NavNode>,
    ) {
        let total = schema.total;
        let loaded = schema.objects.len();
        for (index, kind) in listed_kinds(connection.engine).iter().enumerate() {
            let report = schema.kind_reports.iter().find(|report| report.kind == *kind);
            let objects: Vec<&ObjectRow> = schema.objects.iter().filter(|object| object.kind == *kind).collect();
            // 这一类没读到、也没有任何行：只有当读取明确失败时才留错误节点；
            // 引擎本来没有这一类（kind_sql 返回空集）就不画空分组。
            let failed = report.and_then(|report| report.error.clone());
            if objects.is_empty() && failed.is_none() && !connection.outline_only {
                continue;
            }
            let group_id = group_node_id(&connection.id, database, &schema.name, *kind);
            let mut group = NavNode::new(NavKind::Group, group_id.clone(), String::new());
            group.label_key = Some(kind_label_key(*kind));
            group.parent = Some(parent_id.to_owned());
            group.path = schema_path(&connection.name, database, &schema.name, *kind);
            group.connection_id = connection.id.clone();
            group.connection_name = connection.name.clone();
            group.engine = connection.engine;
            group.database = database.to_owned();
            group.schema = schema.name.clone();
            group.name = kind_name(*kind).to_owned();
            group.object_kind = Some(*kind);
            group.error = failed.clone();
            group.expandable = true;
            // 没有按种类细分时，用模式总量判断是否还有下一页。
            let only_kind = schema.objects.iter().all(|object| object.kind == *kind);
            group.total = report
                .map(|report| report.total)
                .unwrap_or_else(|| if only_kind { schema.total.max(objects.len()) } else { objects.len() });
            group.loaded = objects.len();
            // 续读按模式/库的整页游标走，分组只用于展示数量。
            group.has_more = false;
            group.unloaded = connection.outline_only;
            group.paged = connection.outline_only;
            let _ = index;
            self.attach(parent_id, group);
            if failed.is_some() || connection.outline_only {
                // 这一类本轮读取失败：上一轮已读到的对象原样保留（不标 stale、不移除），
                // 原因只挂在分组上；下一次读取成功时再按正常规则判断缺失。
                let kept: Vec<NavNode> = previous.get(&group_id).into_iter()
                    .flat_map(|group| &group.children)
                    .filter_map(|id| previous.get(id))
                    .filter(|old| old.kind == NavKind::Object)
                    .cloned().collect();
                for mut old in kept {
                    let object_id = old.id.clone();
                    old.children = Vec::new();
                    self.attach(&group_id, old);
                    self.restore_details(previous, &object_id);
                }
                if let Some(group) = self.nodes.get_mut(&group_id) {
                    group.loaded = group.children.len();
                    group.total = group.total.max(group.loaded);
                    if connection.outline_only {
                        if let Some(old) = previous.get(&group_id) {
                            group.loaded = old.loaded;
                            group.total = old.total;
                            group.has_more = old.has_more;
                        }
                    }
                }
                continue;
            }
            for object in &objects {
                let object_id = object_node_id(&connection.id, database, &schema.name, object);
                let node = self.object_node(connection, database, &schema.name, object, &object_id);
                self.attach(&group_id, node);
                self.restore_details(previous, &object_id);
            }
            // 已经读到但新目录里没有的对象：先标 stale，下一轮才移除。
            for old in previous.values() {
                if old.kind != NavKind::Object || old.parent.as_deref() != Some(group_id.as_str()) {
                    continue;
                }
                let key = (old.database.clone(), old.schema.clone(), old.name.clone(), old.object_kind);
                let still_there = schema.objects.iter().any(|object| {
                    (database.to_owned(), schema.name.clone(), object.name.clone(), Some(object.kind)) == key
                });
                // 已经标过一次 stale 的对象在这一轮移除；重新出现的对象用新行。
                if still_there || old.stale {
                    continue;
                }
                let mut stale = old.clone();
                stale.stale = true;
                stale.error = None;
                self.attach(&group_id, stale);
            }
        }
        let _ = (total, loaded);
        if let Some(parent) = self.nodes.get_mut(parent_id) {
            parent.total = parent.total.max(schema.total);
            parent.loaded = parent.loaded.max(schema.objects.len());
            parent.has_more = parent.total > parent.loaded;
        }
    }

    fn object_node(&self, connection: &NavConnection, database: &str, schema: &str, object: &ObjectRow, id: &str) -> NavNode {
        let mut node = NavNode::new(NavKind::Object, id.to_owned(), display_name(schema, &object.name));
        node.parent = None;
        node.path = format!("{} / {}", object_path_text(&connection.name, database, schema), object.name);
        node.connection_id = connection.id.clone();
        node.connection_name = connection.name.clone();
        node.engine = connection.engine;
        node.database = database.to_owned();
        node.schema = schema.to_owned();
        node.name = object.name.clone();
        node.object_kind = Some(object.kind);
        node.stale = object.stale;
        node.signature = object.signature.clone();
        node.relation = object.relation.clone();
        node.expandable = matches!(object.kind, ObjectKind::Table | ObjectKind::View);
        node
    }

    fn restore_details(&mut self, previous: &BTreeMap<String, NavNode>, object_id: &str) {
        let Some(old) = previous.get(object_id) else { return };
        if old.children.is_empty() {
            return;
        }
        let mut new_children = Vec::new();
        for child in &old.children {
            let Some(old_child) = previous.get(child) else { continue };
            let mut copy = old_child.clone();
            copy.children = old_child
                .children
                .iter()
                .filter(|grand| previous.contains_key(*grand))
                .cloned()
                .collect();
            copy.loading = false;
            new_children.push(copy.id.clone());
            self.nodes.insert(copy.id.clone(), copy);
        }
        if let Some(node) = self.nodes.get_mut(object_id) {
            node.children = new_children;
            node.loaded = old.loaded;
            node.has_more = old.has_more;
        }
        // 摘要节点的子行也要一起接回来。
        let ids: Vec<String> = self
            .nodes
            .get(object_id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        for id in ids {
            self.restore_details(previous, &id);
        }
    }

    fn attach(&mut self, parent_id: &str, mut node: NavNode) {
        node.parent = Some(parent_id.to_owned());
        let already_attached = self.nodes.get(&node.id).is_some_and(|old| old.parent.as_deref() == Some(parent_id));
        if let Some(parent) = self.nodes.get_mut(parent_id) {
            if !already_attached {
                parent.children.push(node.id.clone());
            }
        }
        self.nodes.insert(node.id.clone(), node);
    }

    /// 展开/折叠，返回是否真的变化。对象节点展开触发按需子节点读取。
    pub fn toggle(&mut self, id: &str) -> bool {
        let Some(node) = self.nodes.get(id) else { return false };
        if !node.expandable {
            return false;
        }
        if self.expanded.remove(id) {
            return true;
        }
        self.expanded.insert(id.to_owned());
        true
    }

    /// 展开一个节点时把它上面的祖先一起展开，避免出现“展开但不显示”的树。
    pub fn expand(&mut self, id: &str) -> bool {
        let Some(node) = self.nodes.get(id) else { return false };
        if !node.expandable {
            return false;
        }
        let mut chain = vec![id.to_owned()];
        let mut parent = node.parent.clone();
        while let Some(current) = parent {
            chain.push(current.clone());
            parent = self.nodes.get(&current).and_then(|node| node.parent.clone());
        }
        for current in chain {
            self.expanded.insert(current);
        }
        true
    }

    pub fn collapse(&mut self, id: &str) -> bool {
        self.expanded.remove(id)
    }

    /// 对象节点是否需要按需读取列/索引。
    pub fn needs_details(&self, id: &str) -> bool {
        let Some(node) = self.nodes.get(id) else { return false };
        node.kind == NavKind::Object && node.expandable && node.children.is_empty() && !node.loading
    }

    pub fn begin_details(&mut self, id: &str) -> bool {
        let Some(node) = self.nodes.get_mut(id) else { return false };
        if node.loading {
            return false;
        }
        node.loading = true;
        node.error = None;
        true
    }

    /// 库节点是否需要按库读一次 schema 清单（会话当前库以外的库）。
    pub fn needs_database(&self, id: &str) -> bool {
        self.nodes.get(id).is_some_and(|node| {
            node.kind == NavKind::Database && node.unloaded && !node.loading && node.error.is_none()
        })
    }

    /// 库节点开始按库读取：加载态只落在这个库上。
    pub fn begin_database(&mut self, id: &str) -> bool {
        let Some(node) = self.nodes.get_mut(id) else { return false };
        if node.kind != NavKind::Database || node.loading {
            return false;
        }
        node.loading = true;
        node.error = None;
        true
    }

    /// 按库读到的 schema 清单：成功补上这个库的 schema 子节点（它们仍按层读对象），
    /// 失败只在这个库节点上留原因，别的库和连接行都不动。
    pub fn set_database(&mut self, database_id: &str, schemas: Result<Vec<SchemaNode>, String>) -> bool {
        let Some(node) = self.nodes.get(database_id).cloned() else { return false };
        if node.kind != NavKind::Database {
            return false;
        }
        let items = match schemas {
            Ok(items) => items,
            Err(message) => {
                if let Some(node) = self.nodes.get_mut(database_id) {
                    node.loading = false;
                    node.error = Some(message);
                }
                return false;
            }
        };
        let mut connection = NavConnection::new(
            node.connection_id.clone(),
            node.connection_name.clone(),
            node.engine,
            ConnStatus::Connected,
        );
        // 按库读取只给「库 → 模式 → 种类分组」这一层：对象仍按分组分页按需读。
        connection.outline_only = node.engine.shows_schemas();
        let previous = BTreeMap::new();
        for schema in &items {
            self.push_schema(&connection, database_id, &node.database, schema, &previous);
        }
        if let Some(node) = self.nodes.get_mut(database_id) {
            node.loading = false;
            node.error = None;
            node.unloaded = false;
            node.total = items.len();
            node.loaded = node.children.len();
        }
        !items.is_empty()
    }

    /// 列/索引读取结果。失败时只在这个对象节点上留原因，兄弟节点不动。
    pub fn set_details(&mut self, id: &str, columns: Result<Vec<ColumnInfo>, String>, indexes: Result<Vec<IndexInfo>, String>) -> bool {
        let Some(object) = self.nodes.get(id).cloned() else { return false };
        if object.kind != NavKind::Object {
            return false;
        }
        let mut new_children = Vec::new();
        let mut first_error: Option<String> = None;
        let col_group = column_group_id(id);
        match columns {
            Ok(items) => {
                let mut group = NavNode::new(NavKind::ColumnGroup, col_group.clone(), String::new());
                group.label_key = Some("catalog.group.columns");
                group.parent = Some(id.to_owned());
                group.path = format!("{} / columns", object.path);
                group.connection_id = object.connection_id.clone();
                group.connection_name = object.connection_name.clone();
                group.engine = object.engine;
                group.database = object.database.clone();
                group.schema = object.schema.clone();
                group.name = "columns".to_owned();
                group.expandable = true;
                group.total = items.len();
                group.loaded = items.len();
                self.nodes.insert(col_group.clone(), group);
                new_children.push(col_group.clone());
                for item in items {
                    let col_id = column_node_id(id, &item.name);
                    let mut node = NavNode::new(NavKind::Column, col_id.clone(), item.name.clone());
                    node.parent = Some(col_group.clone());
                    node.path = format!("{}.{}", object.path, item.name);
                    node.connection_id = object.connection_id.clone();
                    node.connection_name = object.connection_name.clone();
                    node.engine = object.engine;
                    node.database = object.database.clone();
                    node.schema = object.schema.clone();
                    node.name = item.name.clone();
                    node.label_key = None;
                    node.primary = item.primary;
                    let detail = if item.data_type.is_empty() { String::new() } else { item.data_type.clone() };
                    let marks = [item.primary.then_some("pk"), (!item.nullable).then_some("not null")]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    node.cursor = Some(if detail.is_empty() { marks } else if marks.is_empty() { detail } else { format!("{detail} · {marks}") });
                    self.nodes.insert(col_id.clone(), node);
                    if let Some(group) = self.nodes.get_mut(&col_group) {
                        group.children.push(col_id);
                    }
                }
            }
            Err(message) => first_error = Some(message),
        }
        let idx_group = index_group_id(id);
        match indexes {
            Ok(items) => {
                let mut group = NavNode::new(NavKind::IndexGroup, idx_group.clone(), String::new());
                group.label_key = Some("catalog.group.indexes");
                group.parent = Some(id.to_owned());
                group.path = format!("{} / indexes", object.path);
                group.connection_id = object.connection_id.clone();
                group.connection_name = object.connection_name.clone();
                group.engine = object.engine;
                group.database = object.database.clone();
                group.schema = object.schema.clone();
                group.name = "indexes".to_owned();
                group.expandable = true;
                group.total = items.len();
                group.loaded = items.len();
                self.nodes.insert(idx_group.clone(), group);
                new_children.push(idx_group.clone());
                for item in items {
                    let index_id = index_node_id(id, &item.name);
                    let mut node = NavNode::new(NavKind::Index, index_id.clone(), item.name.clone());
                    node.parent = Some(idx_group.clone());
                    node.path = format!("{}.{}", object.path, item.name);
                    node.connection_id = object.connection_id.clone();
                    node.connection_name = object.connection_name.clone();
                    node.engine = object.engine;
                    node.database = object.database.clone();
                    node.schema = object.schema.clone();
                    node.name = item.name.clone();
                    let flags = [item.primary.then_some("primary"), item.unique.then_some("unique")]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    node.cursor = Some(if flags.is_empty() { item.columns.join(", ") } else { format!("{} · {flags}", item.columns.join(", ")) });
                    self.nodes.insert(index_id.clone(), node);
                    if let Some(group) = self.nodes.get_mut(&idx_group) {
                        group.children.push(index_id);
                    }
                }
            }
            Err(message) => {
                if first_error.is_none() {
                    first_error = Some(message);
                }
            }
        }
        if let Some(node) = self.nodes.get_mut(id) {
            node.loading = false;
            node.children = new_children;
            node.loaded = node.children.len();
            node.error = first_error;
        }
        true
    }

    /// 只有目录读取失败时才调用；保留已经读到的对象和它们的子节点。
    pub fn set_error(&mut self, id: &str, message: impl Into<String>) -> bool {
        let Some(node) = self.nodes.get_mut(id) else { return false };
        node.loading = false;
        node.error = Some(message.into());
        true
    }

    /// 续读：在已有对象之后追加一页。追加失败保留已有对象，只把原因留在该节点。
    pub fn append_objects(&mut self, scope_id: &str, objects: &[ObjectRow], total: usize, error: Option<String>) -> bool {
        let Some(scope) = self.nodes.get(scope_id).cloned() else { return false };
        // 没有库/模式层的引擎（SQLite）把对象直接挂在连接下：续读沿用已有分组的
        // 真实库/模式，否则会另造一套空 database/schema，重复出分组和对象。
        let (database, schema) = match scope.kind {
            NavKind::Connection | NavKind::Keyspace => {
                self.storage_identity_under(scope_id).unwrap_or((scope.database.clone(), scope.schema.clone()))
            }
            _ => (scope.database.clone(), scope.schema.clone()),
        };
        if let Some(message) = error {
            if let Some(node) = self.nodes.get_mut(scope_id) {
                node.loading = false;
                node.error = Some(message);
            }
            return true;
        }
        let mut added = false;
        for object in objects {
            let group_id = group_node_id(&scope.connection_id, &database, &schema, object.kind);
            if !self.nodes.contains_key(&group_id) {
                let mut group = NavNode::new(NavKind::Group, group_id.clone(), String::new());
                group.label_key = Some(kind_label_key(object.kind));
                group.parent = Some(scope_id.to_owned());
                group.path = scope.path.clone();
                group.connection_id = scope.connection_id.clone();
                group.connection_name = scope.connection_name.clone();
                group.engine = scope.engine;
                group.database = database.clone();
                group.schema = schema.clone();
                group.name = kind_name(object.kind).to_owned();
                group.object_kind = Some(object.kind);
                group.expandable = true;
                self.attach(scope_id, group);
            }
            let object_id = object_node_id(&scope.connection_id, &database, &schema, object);
            if self.nodes.contains_key(&object_id) {
                continue;
            }
            let connection = NavConnection::new(scope.connection_id.clone(), scope.connection_name.clone(), scope.engine, ConnStatus::Connected);
            let node = self.object_node(&connection, &database, &schema, object, &object_id);
            self.attach(&group_id, node);
            added = true;
        }
        if let Some(node) = self.nodes.get_mut(scope_id) {
            node.loading = false;
            node.error = None;
            node.loaded += objects.len();
            node.total = total.max(node.loaded);
            node.has_more = node.loaded < node.total;
        }
        added
    }

    /// 某一类对象读取失败的局部重试：只更新这个分组，成功就清掉它自己的错误。
    /// 返回 true 表示这次真的读到了对象。
    pub fn set_kind_page(&mut self, group_id: &str, objects: &[ObjectRow], total: usize, error: Option<String>) -> bool {
        let Some(group) = self.nodes.get(group_id).cloned() else { return false };
        if group.kind != NavKind::Group {
            return false;
        }
        if let Some(message) = error {
            if let Some(node) = self.nodes.get_mut(group_id) {
                node.loading = false;
                node.error = Some(message);
            }
            return false;
        }
        let Some(kind) = group.object_kind else { return false };
        let connection = NavConnection::new(
            group.connection_id.clone(),
            group.connection_name.clone(),
            group.engine,
            ConnStatus::Connected,
        );
        let mut added = 0usize;
        let mut read_ids = BTreeSet::new();
        let of_kind: Vec<&ObjectRow> = objects.iter().filter(|object| object.kind == kind).collect();
        for object in &of_kind {
            let object_id = object_node_id(&group.connection_id, &group.database, &group.schema, object);
            read_ids.insert(object_id.clone());
            if let Some(existing) = self.nodes.get_mut(&object_id) {
                // 上一轮标记过删除的对象这次又读到了：恢复成可用行，不再另建一条。
                existing.stale = false;
                continue;
            }
            let node = self.object_node(&connection, &group.database, &group.schema, object, &object_id);
            self.attach(group_id, node);
            added += 1;
        }
        // 这一类整页读完（没有下一页）时，读不到的旧对象就是被删掉的：
        // 和整树刷新一样先标 stale，下一次刷新仍不在才移除。只读到一页时不判断。
        if of_kind.len() >= total {
            self.retire_missing(group_id, &read_ids);
        }
        if let Some(node) = self.nodes.get_mut(group_id) {
            node.loading = false;
            node.error = None;
            node.loaded = node.children.len();
            node.total = total.max(node.loaded);
        }
        added > 0
    }

    /// 按种类的独立游标。失败保留旧页；成功刷新仅替换这一组已读取窗口。
    pub fn set_kind_slice(&mut self, group_id: &str, objects: &[ObjectRow], total: usize, offset: usize, error: Option<String>) {
        if let Some(error) = error {
            self.set_error(group_id, error);
            return;
        }
        let Some(group) = self.nodes.get(group_id).cloned() else { return };
        if offset == 0 {
            let incoming: BTreeSet<String> = objects.iter().map(|object| object_node_id(&group.connection_id, &group.database, &group.schema, object)).collect();
            let removed: Vec<_> = group.children.iter().filter(|id| !incoming.contains(*id)).cloned().collect();
            if let Some(parent) = self.nodes.get_mut(group_id) { parent.children.retain(|id| incoming.contains(id)); }
            for old in removed {
                // 父列表一次过滤，避免删除大页时逐项扫描同一个子列表。
                if let Some(node) = self.nodes.get_mut(&old) { node.parent = None; }
                self.remove_subtree(&old);
            }
            if self.focus.as_ref().is_none_or(|id| !self.nodes.contains_key(id)) { self.focus = Some(group_id.to_owned()); }
        }
        self.append_objects(group_id, objects, total, None);
        if let Some(node) = self.nodes.get_mut(group_id) {
            node.unloaded = false;
            node.loaded = offset.saturating_add(objects.len());
            node.total = total.max(node.loaded);
            node.has_more = !objects.is_empty() && node.loaded < node.total;
        }
    }

    pub fn needs_group(&self, id: &str) -> bool {
        self.nodes.get(id).is_some_and(|node| node.kind == NavKind::Group && node.unloaded && !node.loading && node.error.is_none())
    }

    /// 一个分组下没有在完整读取里出现的对象：第一次标 stale，已经 stale 的这次移除。
    fn retire_missing(&mut self, group_id: &str, read_ids: &BTreeSet<String>) {
        let children = self.nodes.get(group_id).map(|node| node.children.clone()).unwrap_or_default();
        for child in children {
            if read_ids.contains(&child) {
                continue;
            }
            let Some(node) = self.nodes.get_mut(&child) else { continue };
            if node.kind != NavKind::Object {
                continue;
            }
            if node.stale {
                self.remove_subtree(&child);
            } else {
                node.stale = true;
                node.error = None;
                node.loading = false;
            }
        }
        if let Some(group) = self.nodes.get_mut(group_id) {
            group.loaded = group.children.len();
        }
    }

    /// 把一个节点连同它的子孙从树上拿掉，父节点、展开和焦点一起收拾。
    fn remove_subtree(&mut self, id: &str) {
        let Some(node) = self.nodes.remove(id) else { return };
        if let Some(parent) = node.parent.as_deref().and_then(|parent| self.nodes.get_mut(parent)) {
            parent.children.retain(|child| child != id);
        }
        self.expanded.remove(id);
        if self.focus.as_deref() == Some(id) {
            self.focus = node.parent.clone();
        }
        for child in node.children {
            self.remove_subtree(&child);
        }
    }

    /// 已知被删掉或改名的对象（本应用刚执行成功）：直接从树上拿掉，不再经过 stale。
    pub fn forget(&mut self, id: &str) -> bool {
        if !self.nodes.contains_key(id) {
            return false;
        }
        let parent = self.nodes.get(id).and_then(|node| node.parent.clone());
        self.remove_subtree(id);
        if let Some(group) = parent.as_deref().and_then(|parent| self.nodes.get_mut(parent)) {
            if group.kind == NavKind::Group {
                group.loaded = group.children.len();
                group.total = group.total.saturating_sub(1).max(group.loaded);
            }
        }
        true
    }

    /// 库/模式的“刷新此节点”：从第一页重新读这个范围的对象。
    /// 失败时保留已有子项，只把原因留在这个节点；成功时补上新对象，
    /// 读完整个范围时把不见的旧对象按整树刷新的规则标 stale/移除。展开状态不动。
    /// `kind_reports` 里读取失败的种类：这一类的已有对象原样保留、不参与缺失判断，
    /// 原因挂在这一类的分组上（分组不存在时补一个只带错误的分组）。
    pub fn reload_scope(
        &mut self,
        scope_id: &str,
        objects: &[ObjectRow],
        total: usize,
        error: Option<String>,
        kind_reports: &[KindReport],
    ) -> bool {
        let Some(scope) = self.nodes.get(scope_id).cloned() else { return false };
        if let Some(message) = error {
            if let Some(node) = self.nodes.get_mut(scope_id) {
                node.loading = false;
                node.error = Some(message);
            }
            return false;
        }
        let full = objects.len() >= total;
        let mut read: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let connection = NavConnection::new(scope.connection_id.clone(), scope.connection_name.clone(), scope.engine, ConnStatus::Connected);
        let mut added = false;
        // 读取失败的种类：分组 id → 原因。
        let mut failed: BTreeMap<String, String> = BTreeMap::new();
        for report in kind_reports {
            if let Some(message) = report.error.clone() {
                let group_id = self.ensure_scope_group(scope_id, &scope, report.kind);
                failed.insert(group_id, message);
            }
        }
        for object in objects {
            let group_id = self.ensure_scope_group(scope_id, &scope, object.kind);
            if failed.contains_key(&group_id) {
                continue;
            }
            let object_id = object_node_id(&scope.connection_id, &scope.database, &scope.schema, object);
            read.entry(group_id.clone()).or_default().insert(object_id.clone());
            if let Some(existing) = self.nodes.get_mut(&object_id) {
                existing.stale = false;
                continue;
            }
            let node = self.object_node(&connection, &scope.database, &scope.schema, object, &object_id);
            self.attach(&group_id, node);
            added = true;
        }
        if full {
            let groups: Vec<String> = scope
                .children
                .iter()
                .filter(|id| self.nodes.get(*id).is_some_and(|node| node.kind == NavKind::Group))
                .cloned()
                .collect();
            let empty = BTreeSet::new();
            for group in groups {
                if failed.contains_key(&group) {
                    continue;
                }
                let ids = read.get(&group).unwrap_or(&empty).clone();
                self.retire_missing(&group, &ids);
            }
        }
        for (group_id, ids) in &read {
            if let Some(group) = self.nodes.get_mut(group_id) {
                group.loaded = group.children.len();
                group.total = group.total.max(ids.len());
                group.error = None;
            }
        }
        for (group_id, message) in failed {
            if let Some(group) = self.nodes.get_mut(&group_id) {
                group.loading = false;
                group.error = Some(message);
                group.loaded = group.children.len();
                group.total = group.total.max(group.loaded);
            }
        }
        if let Some(node) = self.nodes.get_mut(scope_id) {
            node.loading = false;
            node.error = None;
            node.loaded = if full { objects.len() } else { scope.loaded.max(objects.len()) };
            node.total = total.max(node.loaded);
            node.has_more = node.loaded < node.total;
        }
        added
    }

    /// 范围节点下某一类对象的分组；还没有时补一个空分组。返回分组 id。
    fn ensure_scope_group(&mut self, scope_id: &str, scope: &NavNode, kind: ObjectKind) -> String {
        let group_id = group_node_id(&scope.connection_id, &scope.database, &scope.schema, kind);
        if !self.nodes.contains_key(&group_id) {
            let mut group = NavNode::new(NavKind::Group, group_id.clone(), String::new());
            group.label_key = Some(kind_label_key(kind));
            group.path = schema_path(&scope.connection_name, &scope.database, &scope.schema, kind);
            group.connection_id = scope.connection_id.clone();
            group.connection_name = scope.connection_name.clone();
            group.engine = scope.engine;
            group.database = scope.database.clone();
            group.schema = scope.schema.clone();
            group.name = kind_name(kind).to_owned();
            group.object_kind = Some(kind);
            group.expandable = true;
            self.attach(scope_id, group);
        }
        group_id
    }

    /// 对象节点下已经读到的列：(列名, 是否主键)。列还没读过时返回 None。
    pub fn object_columns(&self, object_id: &str) -> Option<Vec<(String, bool)>> {
        let object = self.nodes.get(object_id)?;
        let group = object
            .children
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .find(|node| node.kind == NavKind::ColumnGroup)?;
        Some(
            group
                .children
                .iter()
                .filter_map(|id| self.nodes.get(id))
                .filter(|node| node.kind == NavKind::Column)
                .map(|node| (node.name.clone(), node.primary))
                .collect(),
        )
    }

    /// 分组重试开始：只在这个分组上显示加载态，兄弟分组不受影响。
    pub fn begin_group(&mut self, group_id: &str) -> bool {
        let Some(node) = self.nodes.get_mut(group_id) else { return false };
        if node.kind != NavKind::Group || node.loading {
            return false;
        }
        node.loading = true;
        node.error = None;
        true
    }

    /// 没有库/模式层的引擎把对象挂在连接下：从已读到的对象分组借回真实库/模式。
    fn storage_identity_under(&self, scope_id: &str) -> Option<(String, String)> {
        self.nodes.get(scope_id).and_then(|scope| {
            scope
                .children
                .iter()
                .filter_map(|id| self.nodes.get(id))
                .find(|child| child.kind == NavKind::Group)
                .map(|child| (child.database.clone(), child.schema.clone()))
        })
    }

    /// 记录展开意图：节点还没读进树时也保留，下一次 apply 有这条连接就会展开。
    pub fn expand_reserved(&mut self, id: &str) -> bool {
        self.expanded.insert(id.to_owned())
    }

    /// 续读开始时把加载状态放在这个范围节点上，别处不受影响。
    pub fn begin_scope(&mut self, scope_id: &str) {
        if let Some(node) = self.nodes.get_mut(scope_id) {
            node.loading = true;
            node.error = None;
        }
    }

    pub fn set_page_state(&mut self, scope_id: &str, total: usize, has_more: bool) {
        if let Some(node) = self.nodes.get_mut(scope_id) {
            node.total = total;
            node.has_more = has_more;
        }
    }

    /// Redis SCAN 游标：0/空表示这一轮读完。
    pub fn set_cursor(&mut self, id: &str, cursor: Option<String>, loaded: usize) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.loading = false;
            node.cursor = cursor;
            node.loaded = loaded;
        }
    }

    /// 普通资源管理器只遍历本连接展开的子树，避免逐连接重建整棵树。
    ///
    /// 有筛选词时按同一份筛选规则只走进已加载节点：命中的对象连同它的库/模式/种类
    /// 分组一起保留（祖先即“命中及祖先”），并且不受折叠状态限制——筛选期间这条连接
    /// 临时展开，清空筛选后回到原来的展开现场。
    pub fn visible_under(&self, root_id: &str) -> Vec<NavRow> {
        let mut rows = Vec::new();
        let needle = self.query.trim().to_lowercase();
        let expanded = self.expanded.contains(root_id) || !needle.is_empty();
        if expanded {
            if let Some(root) = self.nodes.get(root_id) {
                for child in &root.children {
                    if needle.is_empty() {
                        self.walk(child, 0, &mut rows);
                    } else {
                        self.walk_filtered(child, 0, &needle, &mut rows);
                    }
                }
            }
        }
        rows
    }

    /// 可见行。没有搜索词时按展开状态展开整棵树；有搜索词时保留树结构：
    /// 命中的对象连同它的连接 / 库 / 模式 / 分组一起显示，祖先自动展开，命中处高亮。
    pub fn visible(&self) -> Vec<NavRow> {
        let mut rows = Vec::new();
        if self.query.trim().is_empty() {
            for id in &self.roots {
                self.walk(id, 0, &mut rows);
            }
            return rows;
        }
        let needle = self.query.trim().to_lowercase();
        for id in &self.roots {
            self.walk_filtered(id, 0, &needle, &mut rows);
        }
        rows
    }

    /// 只有对象、Redis 键空间、库和模式按名字参与筛选；分组与列不单独命中，
    /// 否则“列”“表”这样的分组名或常见列名会把整棵树都带出来。
    fn name_hits(node: &NavNode, needle: &str) -> bool {
        matches!(node.kind, NavKind::Object | NavKind::Keyspace | NavKind::Database | NavKind::Schema)
            && node.name.to_lowercase().contains(needle)
    }

    fn subtree_hits(&self, id: &str, needle: &str) -> bool {
        let Some(node) = self.nodes.get(id) else { return false };
        Self::name_hits(node, needle) || node.children.iter().any(|child| self.subtree_hits(child, needle))
    }

    fn walk_filtered(&self, id: &str, depth: usize, needle: &str, rows: &mut Vec<NavRow>) {
        let Some(node) = self.nodes.get(id) else { return };
        if Self::name_hits(node, needle) && node.kind == NavKind::Object {
            // 命中的对象本身照常显示：用户展开它就能看到列和索引。
            let at = rows.len();
            self.walk(id, depth, rows);
            if let Some(row) = rows.get_mut(at) {
                row.highlight = match_span(&row.label, needle);
            }
            return;
        }
        if !self.subtree_hits(id, needle) {
            return;
        }
        let mut row = self.row_of(node, depth, true);
        if Self::name_hits(node, needle) {
            row.highlight = match_span(&row.label, needle);
        }
        rows.push(row);
        for child in &node.children {
            self.walk_filtered(child, depth + 1, needle, rows);
        }
    }

    fn row_of(&self, node: &NavNode, depth: usize, expanded: bool) -> NavRow {
        let label = if node.label_key.is_some() {
            node.label_key.unwrap_or_default().to_owned()
        } else {
            node.label.clone()
        };
        NavRow {
            id: node.id.clone(),
            depth,
            kind: node.kind,
            highlight: None,
            label,
            name: node.name.clone(),
            label_key: node.label_key,
            path: node.path.clone(),
            expanded,
            focused: self.focus.as_deref() == Some(node.id.as_str()),
            expandable: node.expandable,
            object_kind: node.object_kind,
            stale: node.stale,
            loading: node.loading,
            unloaded: node.unloaded,
            error: node.error.clone(),
            has_more: node.has_more,
            total: node.total,
            loaded: node.loaded,
            detail: if matches!(node.kind, NavKind::Column | NavKind::Index) { node.cursor.clone().unwrap_or_default() } else { String::new() },
            primary: node.primary,
        }
    }

    fn walk(&self, id: &str, depth: usize, rows: &mut Vec<NavRow>) {
        let Some(node) = self.nodes.get(id) else { return };
        let expanded = self.expanded.contains(id);
        rows.push(self.row_of(node, depth, expanded));
        if !expanded {
            return;
        }
        for child in &node.children {
            self.walk(child, depth + 1, rows);
        }
    }

    pub fn set_focus(&mut self, id: &str) -> bool {
        if !self.nodes.contains_key(id) {
            return false;
        }
        self.focus = Some(id.to_owned());
        true
    }

    pub fn focus_at(&mut self, index: usize) {
        let rows = self.visible();
        if let Some(row) = rows.get(index.min(rows.len().saturating_sub(1))) {
            self.focus = Some(row.id.clone());
        }
    }

    pub fn focus_index(&self) -> Option<usize> {
        let focus = self.focus.as_deref()?;
        self.visible().iter().position(|row| row.id == focus)
    }

    /// 键盘移动：上下在可见行里走，Home/End 到两端。
    pub fn move_focus(&mut self, step: isize) {
        let rows = self.visible();
        if rows.is_empty() {
            self.focus = None;
            return;
        }
        let current = self.focus_index().unwrap_or(0) as isize;
        let next = (current + step).clamp(0, rows.len() as isize - 1);
        self.focus = Some(rows[next as usize].id.clone());
    }

    pub fn focus_first(&mut self) {
        self.focus = self.visible().first().map(|row| row.id.clone());
    }

    pub fn focus_last(&mut self) {
        self.focus = self.visible().last().map(|row| row.id.clone());
    }

    /// 右方向键：分组/连接展开，对象读子节点，列直接插入。
    pub fn activate(&mut self, id: &str) -> NavAction {
        let Some(node) = self.nodes.get(id).cloned() else { return NavAction::None };
        match node.kind {
            NavKind::Connection | NavKind::Database | NavKind::Schema | NavKind::Group => {
                if self.expanded.insert(node.id.clone()) {
                    NavAction::Load(node.id.clone())
                } else {
                    NavAction::None
                }
            }
            NavKind::Keyspace => NavAction::OpenKeyspace {
                connection_id: node.connection_id.clone(),
                keyspace: node.name.clone(),
            },
            NavKind::Object => {
                let Some(object) = node.object() else { return NavAction::None };
                if node.engine == Engine::Mongodb {
                    return NavAction::OpenDocuments(node.id.clone());
                }
                if object.opens_data() {
                    if self.needs_details(&node.id) {
                        let _ = self.expand(&node.id);
                        return NavAction::Load(node.id.clone());
                    }
                    NavAction::OpenData(node.id.clone())
                } else {
                    NavAction::OpenStructure(node.id.clone())
                }
            }
            NavKind::Column => NavAction::InsertColumn {
                object_id: node.parent.as_deref().and_then(|group| self.nodes.get(group)).and_then(|group| group.parent.clone()).unwrap_or_default(),
                column: node.name.clone(),
            },
            NavKind::ColumnGroup | NavKind::IndexGroup | NavKind::Index => {
                self.toggle(&node.id);
                NavAction::None
            }
        }
    }

    /// 鼠标双击/回车：容器行在展开与折叠之间切换，其它行照 activate 走。
    /// 键盘右方向键仍然只展开，不在这里折叠。
    pub fn activate_or_toggle(&mut self, id: &str) -> NavAction {
        let Some((kind, expandable)) = self.nodes.get(id).map(|node| (node.kind, node.expandable)) else {
            return NavAction::None;
        };
        let containers = matches!(
            kind,
            NavKind::Connection | NavKind::Database | NavKind::Schema | NavKind::Group | NavKind::ColumnGroup | NavKind::IndexGroup
        );
        if containers && expandable {
            if self.expanded.remove(id) {
                return NavAction::None;
            }
            self.expand(id);
            return match kind {
                NavKind::ColumnGroup | NavKind::IndexGroup => NavAction::None,
                _ => NavAction::Load(id.to_owned()),
            };
        }
        self.activate(id)
    }

    /// 左方向键：已展开就折叠，否则回到父节点。
    pub fn retreat(&mut self, id: &str) -> NavAction {
        let Some(node) = self.nodes.get(id).cloned() else { return NavAction::None };
        if node.expandable && self.expanded.remove(&node.id) {
            return NavAction::None;
        }
        match node.parent {
            Some(parent) => {
                self.focus = Some(parent);
                NavAction::None
            }
            None => NavAction::None,
        }
    }

    /// 搜索：先记住展开与焦点，清除后原样恢复。
    pub fn search(&mut self, query: &str) -> bool {
        let trimmed = query.trim().to_owned();
        if trimmed == self.query {
            return false;
        }
        if self.query.is_empty() && !trimmed.is_empty() {
            self.saved_expansion = Some(self.expanded.clone());
            self.saved_focus = self.focus.clone();
        }
        self.query = trimmed;
        if !self.query.is_empty() {
            self.focus = self.visible().first().map(|row| row.id.clone());
        } else {
            self.restore_view();
        }
        true
    }

    pub fn clear_search(&mut self) -> bool {
        self.search("")
    }

    fn restore_view(&mut self) {
        if let Some(expansion) = self.saved_expansion.take() {
            self.expanded = expansion;
        }
        if let Some(focus) = self.saved_focus.take() {
            if focus.is_empty() || self.nodes.contains_key(&focus) {
                self.focus = Some(focus);
            }
        }
    }

    /// 当前焦点节点的对象身份；搜索命中或树里选中的行都可以用。
    pub fn focused_object(&self) -> Option<ObjectKey> {
        self.focus_node().and_then(|node| node.object())
    }

    /// 被删除（stale）的对象不再当作可打开目标。
    pub fn target_usable(&self, id: &str) -> bool {
        self.nodes.get(id).is_some_and(|node| !node.stale)
    }

    /// 目录里是否还有这个对象身份；用于在动作前确认目标仍然存在。
    pub fn object_by_key(&self, connection_id: &str, database: &str, schema: &str, name: &str, kind: ObjectKind) -> Option<&NavNode> {
        self.nodes.values().find(|node| {
            node.kind == NavKind::Object
                && !node.stale
                && node.connection_id == connection_id
                && node.database == database
                && node.schema == schema
                && node.name == name
                && node.object_kind == Some(kind)
        })
    }
}

fn display_name(schema: &str, name: &str) -> String {
    if schema.is_empty() || schema == "dbo" || schema == "public" {
        name.to_owned()
    } else {
        format!("{schema}.{name}")
    }
}

fn object_path_text(connection: &str, database: &str, schema: &str) -> String {
    let mut parts = vec![connection.to_owned()];
    if !database.is_empty() {
        parts.push(database.to_owned());
    }
    if !schema.is_empty() {
        parts.push(schema.to_owned());
    }
    parts.join(" / ")
}

fn schema_path(connection: &str, database: &str, schema: &str, kind: ObjectKind) -> String {
    format!("{} / {}", object_path_text(connection, database, schema), kind_name(kind))
}

pub fn kind_label_key(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "catalog.kind.table",
        ObjectKind::View => "catalog.kind.view",
        ObjectKind::Function => "catalog.kind.function",
        ObjectKind::Procedure => "catalog.kind.procedure",
        ObjectKind::Trigger => "catalog.kind.trigger",
        ObjectKind::Sequence => "catalog.kind.sequence",
        ObjectKind::Enum => "catalog.kind.enum",
        ObjectKind::Domain => "catalog.kind.domain",
        ObjectKind::Extension => "catalog.kind.extension",
        ObjectKind::Event => "catalog.kind.event",
    }
}

/// 搜索命中的第一段区间（字节偏移），大小写不敏感。
pub fn match_span(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return None;
    }
    let lower = haystack.to_lowercase();
    let at = lower.find(&needle.to_lowercase())?;
    // 小写化可能改变字节长度（极少数字符）；只在长度一致时给区间，避免切坏 UTF-8。
    if lower.len() != haystack.len() {
        return None;
    }
    Some((at, at + needle.len().min(haystack.len().saturating_sub(at))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::model::KindReport;

    fn table(name: &str) -> ObjectRow {
        ObjectRow::table(name, Some(3))
    }

    fn schema(name: &str, objects: Vec<ObjectRow>) -> SchemaNode {
        let total = objects.len();
        SchemaNode {
            name: name.to_owned(),
            objects,
            total,
            error: None,
            kind_reports: Vec::new(),
        }
    }

    fn database(name: &str, schemas: Vec<SchemaNode>) -> DatabaseNode {
        DatabaseNode {
            name: name.to_owned(),
            schemas,
            error: None,
        }
    }

    fn connection(engine: Engine, databases: Vec<DatabaseNode>) -> NavConnection {
        let mut connection = NavConnection::new(format!("conn-{}", engine.as_str()), engine.as_str(), engine, ConnStatus::Connected);
        connection.databases = databases;
        connection
    }

    fn labels(tree: &NavTree) -> Vec<(NavKind, String)> {
        tree.visible().into_iter().map(|row| (row.kind, row.label)).collect()
    }

    /// 会话当前库以外的库只带回来一个库名（没有错误、没有模式）：展开这一行时按库读，
    /// 先显示加载态，再按层出现模式/分组/对象。
    #[test]
    fn a_non_current_database_loads_layer_by_layer_with_a_loading_state() {
        let mut pg = connection(
            Engine::Postgres,
            vec![
                database("shop", vec![schema("public", vec![table("orders")])]),
                database("audit", Vec::new()),
            ],
        );
        pg.outline_only = true;
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        let conn_id = connection_node_id("conn-postgres");
        tree.expand(&conn_id);
        let current = database_node_id("conn-postgres", "shop");
        let other = database_node_id("conn-postgres", "audit");
        let other_node = tree.node(&other).unwrap();
        assert!(other_node.error.is_none(), "别的库不是错误状态");
        assert!(other_node.expandable, "别的库可以展开");
        assert!(other_node.unloaded, "别的库标成还没读");
        assert!(!tree.needs_database(&current), "当前库随连接一次读出，不需要再读");
        assert!(tree.needs_database(&other));

        // 展开：先显示加载态。
        tree.expand(&other);
        assert!(tree.begin_database(&other));
        assert!(tree.node(&other).unwrap().loading);
        assert!(!tree.needs_database(&other), "在途读取不重复发请求");
        assert!(!tree.begin_database(&other));
        let row = tree.visible().into_iter().find(|row| row.id == other).unwrap();
        assert!(row.loading, "库行渲染加载指示");

        // 读回 schema：出现模式与（未读的）对象种类分组。
        tree.set_database(&other, Ok(vec![SchemaNode::empty("public")]));
        let node = tree.node(&other).unwrap();
        assert!(!node.loading && node.error.is_none() && !node.unloaded);
        let schema_id = schema_node_id("conn-postgres", "audit", "public");
        assert!(tree.node(&schema_id).is_some(), "按库读回的 schema 出现在树下");
        let group = group_node_id("conn-postgres", "audit", "public", ObjectKind::Table);
        assert!(tree.needs_group(&group), "对象仍按种类分页按需读");
        tree.begin_group(&group);
        tree.set_kind_page(&group, &[table("login")], 1, None);
        let login = object_node_id("conn-postgres", "audit", "public", &table("login"));
        assert!(tree.node(&login).is_some(), "按种类读回的对象挂在这个分组下");
        tree.expand(&schema_id);
        tree.expand(&group);
        assert!(tree.visible().iter().any(|row| row.id == login));
        // 当前库的树没被这次按库读取改动。
        assert!(tree.node(&schema_node_id("conn-postgres", "shop", "public")).is_some(), "当前库的模式不受影响");
    }

    /// 按库读取失败只在这个库上留原因；重试从同一个库再读，成功后清掉原因。
    #[test]
    fn a_failed_database_read_keeps_the_reason_on_that_database_only() {
        let mut pg = connection(Engine::Postgres, vec![database("audit", Vec::new())]);
        pg.outline_only = true;
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        tree.expand(&connection_node_id("conn-postgres"));
        let other = database_node_id("conn-postgres", "audit");
        tree.begin_database(&other);
        tree.set_database(&other, Err("读取库表失败：permission denied".to_owned()));
        let node = tree.node(&other).unwrap();
        assert!(!node.loading);
        assert_eq!(node.error.as_deref(), Some("读取库表失败：permission denied"));
        assert!(node.unloaded, "失败后仍可按库重试");
        assert!(!tree.needs_database(&other), "带原因的库走重试入口而不是自动重读");
        assert!(tree.node(&connection_node_id("conn-postgres")).unwrap().error.is_none());

        assert!(tree.begin_database(&other));
        assert!(tree.node(&other).unwrap().error.is_none());
        tree.set_database(&other, Ok(vec![SchemaNode::empty("public")]));
        assert!(tree.node(&other).unwrap().error.is_none());
        assert!(!tree.node(&other).unwrap().unloaded);
    }
    #[test]
    fn progressive_connections_preserve_other_inflight_and_loaded_groups() {
        let mut first = NavConnection::new("first", "First", Engine::Mysql, ConnStatus::Connected);
        first.outline_only = true;
        first.databases = vec![DatabaseNode { name: "shop".into(), error: None, schemas: vec![SchemaNode::empty("")] }];
        let mut second = first.clone();
        second.id = "second".into();
        let mut tree = NavTree::new();
        tree.apply(&[first.clone(), second.clone()]);
        let group = group_node_id("first", "shop", "", ObjectKind::Table);
        assert!(tree.begin_group(&group));
        let changed = std::iter::once("second".to_owned()).collect();
        tree.apply_changed(&[first.clone(), second.clone()], &changed);
        assert!(tree.node(&group).unwrap().loading, "另一个连接到达不能清除在途读取");
        assert!(!tree.needs_group(&group));
        tree.set_kind_slice(&group, &[ObjectRow::table("orders", None)], 1, 0, None);
        tree.apply_changed(&[first, second], &changed);
        assert!(!tree.node(&group).unwrap().unloaded);
        assert_eq!(tree.node(&group).unwrap().loaded, 1);
    }

    #[test]
    fn huge_outline_stays_lazy_and_kind_pages_have_independent_cursors() {
        let mut connection = NavConnection::new("huge", "Huge", Engine::Mysql, ConnStatus::Connected);
        connection.outline_only = true;
        connection.databases = vec![DatabaseNode { name: "shop".into(), error: None, schemas: vec![SchemaNode::empty("")] }];
        let mut tree = NavTree::new();
        tree.apply(&[connection.clone()]);
        assert!(tree.nodes.len() < 12, "未知的十万对象目录不创建对象行");
        let group = group_node_id("huge", "shop", "", ObjectKind::Table);
        let views = group_node_id("huge", "shop", "", ObjectKind::View);
        assert!(tree.needs_group(&group));
        assert!(tree.begin_group(&group));
        assert!(!tree.begin_group(&group), "重复展开不产生第二个请求");
        let page: Vec<_> = (0..100).map(|i| ObjectRow::table(format!("table_{i:06}"), None)).collect();
        tree.set_kind_slice(&group, &page, 100_000, 0, None);
        assert_eq!(tree.node(&group).unwrap().loaded, 100);
        assert!(tree.node(&group).unwrap().has_more);
        assert!(!tree.needs_group(&group));
        assert!(tree.needs_group(&views), "读取表不会预读视图、函数或触发器");
        let next: Vec<_> = (100..200).map(|i| ObjectRow::table(format!("table_{i:06}"), None)).collect();
        tree.set_kind_slice(&group, &next, 100_000, 100, Some("slow".into()));
        assert_eq!(tree.node(&group).unwrap().loaded, 100, "失败不推进游标");
        tree.set_kind_slice(&group, &next, 100_000, 100, None);
        assert_eq!(tree.node(&group).unwrap().loaded, 200);
        assert_eq!(tree.node(&group).unwrap().children.len(), 200);
        assert!(tree.nodes.len() < 215);
        tree.apply(&[connection]);
        assert_eq!(tree.node(&group).unwrap().children.len(), 200, "概要刷新期间保留已读内容");
        tree.set_kind_slice(&group, &page, 100_000, 0, None);
        assert_eq!(tree.node(&group).unwrap().loaded, 100);
        assert_eq!(tree.node(&group).unwrap().children.len(), 100, "刷新成功后重新从第一页续读");
    }

    #[test]
    fn relational_hierarchy_follows_each_engine_and_skips_empty_titles() {
        // PostgreSQL：连接 → 库 → 模式 → 对象种类 → 对象。
        let pg = connection(
            Engine::Postgres,
            vec![database("shop", vec![schema("public", vec![table("orders")])])],
        );
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        assert_eq!(tree.roots().len(), 1);
        let conn_id = connection_node_id("conn-postgres");
        assert_eq!(tree.visible().len(), 1, "默认只有连接行");
        tree.expand(&conn_id);
        let db_id = database_node_id("conn-postgres", "shop");
        tree.expand(&db_id);
        let schema_id = schema_node_id("conn-postgres", "shop", "public");
        tree.expand(&schema_id);
        let group_id = group_node_id("conn-postgres", "shop", "public", ObjectKind::Table);
        tree.expand(&group_id);
        let rows = labels(&tree);
        assert_eq!(rows[0].0, NavKind::Connection);
        assert_eq!((rows[1].0, rows[1].1.as_str()), (NavKind::Database, "shop"));
        assert_eq!((rows[2].0, rows[2].1.as_str()), (NavKind::Schema, "public"));
        assert_eq!(rows[3].0, NavKind::Group);
        assert_eq!((rows[4].0, rows[4].1.as_str()), (NavKind::Object, "orders"));

        // MySQL：没有模式层。
        let my = connection(Engine::Mysql, vec![database("shop", vec![schema("", vec![table("orders")])])]);
        let mut tree = NavTree::new();
        tree.apply(&[my]);
        tree.expand(&connection_node_id("conn-mysql"));
        tree.expand(&database_node_id("conn-mysql", "shop"));
        let kinds: Vec<NavKind> = tree.visible().into_iter().map(|row| row.kind).collect();
        assert!(kinds.contains(&NavKind::Database));
        assert!(!kinds.contains(&NavKind::Schema), "MySQL 不画空模式标题");

        // SQLite：连接下面直接是对象种类。
        let lite = connection(Engine::Sqlite, vec![database("main", vec![schema("", vec![table("notes")])])]);
        let mut tree = NavTree::new();
        tree.apply(&[lite]);
        tree.expand(&connection_node_id("conn-sqlite"));
        let kinds: Vec<NavKind> = tree.visible().into_iter().map(|row| row.kind).collect();
        assert!(!kinds.contains(&NavKind::Database), "SQLite 不画库标题");
        assert!(!kinds.contains(&NavKind::Schema));
        assert!(kinds.contains(&NavKind::Group));
    }

    #[test]
    fn routines_and_triggers_never_open_a_data_page() {
        let pg = connection(
            Engine::Postgres,
            vec![database(
                "shop",
                vec![schema(
                    "public",
                    vec![
                        table("orders"),
                        ObjectRow::fresh("touch", ObjectKind::Trigger),
                        ObjectRow::fresh("total", ObjectKind::Function),
                    ],
                )],
            )],
        );
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        let trigger_id = object_node_id("conn-postgres", "shop", "public", &ObjectRow::fresh("touch", ObjectKind::Trigger));
        let function_id = object_node_id("conn-postgres", "shop", "public", &ObjectRow::fresh("total", ObjectKind::Function));
        let table_id = object_node_id("conn-postgres", "shop", "public", &table("orders"));
        assert!(!tree.node(&trigger_id).unwrap().object().unwrap().opens_data());
        assert!(!tree.node(&function_id).unwrap().object().unwrap().opens_data());
        assert!(tree.node(&table_id).unwrap().object().unwrap().opens_data());
        assert_eq!(tree.activate(&trigger_id), NavAction::OpenStructure(trigger_id.clone()));
        assert_eq!(tree.activate(&function_id), NavAction::OpenStructure(function_id));
        // 表先读按需子节点，再打开数据页。
        assert_eq!(tree.activate(&table_id), NavAction::Load(table_id.clone()));
    }

    #[test]
    fn local_kind_failure_keeps_sibling_objects_and_retry_stays_on_that_group() {
        let mut broken = schema("public", vec![table("orders")]);
        broken.kind_reports = vec![
            KindReport {
                kind: ObjectKind::Table,
                total: 1,
                error: None,
            },
            KindReport {
                kind: ObjectKind::Function,
                total: 0,
                error: Some("permission denied for function".to_owned()),
            },
        ];
        let pg = connection(Engine::Postgres, vec![database("shop", vec![broken])]);
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        let table_group = group_node_id("conn-postgres", "shop", "public", ObjectKind::Table);
        let function_group = group_node_id("conn-postgres", "shop", "public", ObjectKind::Function);
        assert!(tree.node(&table_group).unwrap().error.is_none());
        assert_eq!(
            tree.node(&function_group).unwrap().error.as_deref(),
            Some("permission denied for function")
        );
        let object_id = object_node_id("conn-postgres", "shop", "public", &table("orders"));
        assert!(tree.node(&object_id).is_some(), "表对象仍在，失败不清兄弟节点");
    }

    #[test]
    fn stale_objects_are_marked_once_and_removed_on_the_next_refresh() {
        let pg = |objects: Vec<ObjectRow>| connection(Engine::Postgres, vec![database("shop", vec![schema("public", objects)])]);
        let mut tree = NavTree::new();
        tree.apply(&[pg(vec![table("orders"), table("people")])]);
        let orders = object_node_id("conn-postgres", "shop", "public", &table("orders"));
        assert!(!tree.node(&orders).unwrap().stale);
        assert!(tree.target_usable(&orders));

        tree.apply(&[pg(vec![table("people")])]);
        assert!(tree.node(&orders).unwrap().stale, "第一次缺失先标 stale");
        assert!(!tree.target_usable(&orders), "stale 对象不再是可用目标");

        tree.apply(&[pg(vec![table("people")])]);
        assert!(tree.node(&orders).is_none(), "第二次刷新才移除");
        let people = object_node_id("conn-postgres", "shop", "public", &table("people"));
        assert!(!tree.node(&people).unwrap().stale);
    }

    #[test]
    fn same_name_objects_in_different_scopes_have_distinct_identity() {
        let pg = connection(
            Engine::Postgres,
            vec![
                database("shop", vec![schema("public", vec![table("orders")])]),
                database("archive", vec![schema("public", vec![table("orders")])]),
            ],
        );
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        let first = object_node_id("conn-postgres", "shop", "public", &table("orders"));
        let second = object_node_id("conn-postgres", "archive", "public", &table("orders"));
        assert_ne!(first, second);
        assert_eq!(tree.node(&first).unwrap().object().unwrap().database, "shop");
        assert_eq!(tree.node(&second).unwrap().object().unwrap().database, "archive");
    }

    #[test]
    fn continuation_appends_objects_and_failure_keeps_loaded_ones() {
        let mut first = schema("public", vec![table("a")]);
        first.total = 3;
        let pg = connection(Engine::Postgres, vec![database("shop", vec![first])]);
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        let group = group_node_id("conn-postgres", "shop", "public", ObjectKind::Table);
        assert_eq!(tree.node(&group).unwrap().total, 3);

        tree.begin_scope(&group);
        assert!(tree.node(&group).unwrap().loading);
        assert!(tree.append_objects(&group, &[table("b"), table("c")], 3, None));
        let names: Vec<&str> = tree
            .node(&group)
            .unwrap()
            .children
            .iter()
            .filter_map(|id| tree.node(id))
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);

        tree.begin_scope(&group);
        assert!(tree.append_objects(&group, &[], 3, Some("连接中断".to_owned())));
        assert_eq!(tree.node(&group).unwrap().error.as_deref(), Some("连接中断"));
        assert_eq!(tree.node(&group).unwrap().children.len(), 3, "追加失败保留已有对象");
        assert!(!tree.node(&group).unwrap().loading, "失败后离开加载态");
    }


    #[test]
    fn sqlite_continuation_keeps_main_identity_and_never_duplicates_rows() {
        // SQLite 的对象直接挂在连接下：续读延续已有的 main 身份，不另造空库/空模式。
        let sqlite = connection(Engine::Sqlite, vec![database("main", vec![schema("", vec![table("a")])])]);
        let mut tree = NavTree::new();
        tree.apply(&[sqlite]);
        let conn = connection_node_id("conn-sqlite");
        tree.expand(&conn);
        let group = group_node_id("conn-sqlite", "main", "", ObjectKind::Table);
        assert!(tree.node(&group).is_some(), "首屏的 main 分组");

        tree.begin_scope(&conn);
        assert!(tree.append_objects(&conn, &[table("b")], 2, None));
        let groups: Vec<&NavNode> = tree
            .nodes()
            .filter(|node| node.kind == NavKind::Group && node.parent.as_deref() == Some(conn.as_str()))
            .collect();
        assert_eq!(groups.len(), 1, "续读不能再造一个空 library 分组");
        assert_eq!(groups[0].id, group);
        assert_eq!(groups[0].database, "main");

        let names: Vec<String> = tree
            .node(&group)
            .unwrap()
            .children
            .iter()
            .filter_map(|id| tree.node(id))
            .map(|node| format!("{}:{}", node.database, node.name))
            .collect();
        assert_eq!(names, vec!["main:a".to_owned(), "main:b".to_owned()]);
        // 重复的续读结果不再加行。
        assert!(!tree.append_objects(&conn, &[table("b")], 2, None));
        assert_eq!(tree.node(&group).unwrap().children.len(), 2);

        // 分组级重试只补充这一类的对象，并清掉分组自己的错误。
        let view_group = group_node_id("conn-sqlite", "main", "", ObjectKind::View);
        assert!(!tree.set_kind_page(&view_group, &[], 0, None), "不存在的分组不假装成功");
        tree.set_error(&group, "读取失败");
        assert!(tree.begin_group(&group));
        assert!(tree.set_kind_page(&group, &[table("c")], 3, None));
        let node = tree.node(&group).unwrap();
        assert!(node.error.is_none());
        assert_eq!(node.total, 3);
        assert_eq!(node.children.len(), 3);
    }

    #[test]
    fn details_load_columns_and_indexes_with_their_own_errors() {
        let pg = connection(
            Engine::Postgres,
            vec![database("shop", vec![schema("public", vec![table("orders")])])],
        );
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        let object = object_node_id("conn-postgres", "shop", "public", &table("orders"));
        assert!(tree.needs_details(&object));
        assert!(tree.begin_details(&object));
        tree.set_details(
            &object,
            Ok(vec![ColumnInfo {
                name: "id".to_owned(),
                data_type: "integer".to_owned(),
                nullable: false,
                primary: true,
            }]),
            Err("索引读取失败".to_owned()),
        );
        let columns = column_group_id(&object);
        let column = column_node_id(&object, "id");
        assert!(tree.node(&columns).is_some());
        assert_eq!(tree.node(&column).unwrap().name, "id");
        assert_eq!(tree.node(&object).unwrap().error.as_deref(), Some("索引读取失败"));
        assert!(!tree.node(&object).unwrap().loading);

        // 列节点右方向键插入列名，插到对象节点的父链上。
        let action = tree.activate(&column);
        match action {
            NavAction::InsertColumn { object_id, column } => {
                assert_eq!(object_id, object);
                assert_eq!(column, "id");
            }
            other => panic!("列节点应插入列名：{other:?}"),
        }
    }

    #[test]
    fn search_keeps_the_tree_shape_with_highlight_and_restores_the_tree() {
        let pg = connection(
            Engine::Postgres,
            vec![database("shop", vec![schema("public", vec![table("orders"), table("people")])])],
        );
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        let schema_id = schema_node_id("conn-postgres", "shop", "public");
        tree.expand(&schema_id);
        let group = group_node_id("conn-postgres", "shop", "public", ObjectKind::Table);
        tree.expand(&group);
        tree.focus_last();
        let before_focus = tree.focus().unwrap().to_owned();

        assert!(tree.search("ord"));
        let rows = tree.visible();
        // 筛选保留树结构：命中的对象连同连接 / 库 / 模式 / 分组一起出现，未命中的同级对象隐藏。
        let hits: Vec<_> = rows.iter().filter(|row| row.highlight.is_some()).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "orders");
        let (start, end) = hits[0].highlight.expect("命中要高亮");
        assert_eq!(&hits[0].label[start..end].to_lowercase(), "ord");
        assert!(rows.iter().all(|row| row.label != "people"), "未命中的对象不出现");
        for kind in [NavKind::Connection, NavKind::Database, NavKind::Schema, NavKind::Group] {
            assert!(rows.iter().any(|row| row.kind == kind && row.expanded), "祖先 {kind:?} 自动展开");
        }
        let at = rows.iter().position(|row| row.label == "orders").unwrap();
        assert!(rows[at].depth > 0, "命中行保留树缩进");

        assert!(tree.search("nothing-matches"));
        assert!(tree.visible().is_empty(), "没有命中时不留空壳祖先");
        assert!(tree.search("ord"));

        assert!(tree.clear_search());
        assert!(tree.query().is_empty());
        assert!(tree.is_expanded(&group), "清除搜索后展开状态恢复");
        assert_eq!(tree.focus(), Some(before_focus.as_str()));
        assert!(tree.visible().iter().any(|row| row.kind == NavKind::Object));
    }

    #[test]
    fn keyboard_moves_over_visible_rows_and_opens_objects() {
        let pg = connection(
            Engine::Postgres,
            vec![database("shop", vec![schema("public", vec![table("orders")])])],
        );
        let mut tree = NavTree::new();
        tree.apply(&[pg]);
        tree.focus_first();
        assert_eq!(tree.focus(), Some(connection_node_id("conn-postgres").as_str()));
        // 右方向键先展开连接。
        assert_eq!(tree.activate(&connection_node_id("conn-postgres")), NavAction::Load(connection_node_id("conn-postgres")));
        tree.move_focus(1);
        let database = database_node_id("conn-postgres", "shop");
        assert_eq!(tree.focus(), Some(database.as_str()));
        // 折叠后焦点回到父节点。
        tree.collapse(&database);
        tree.retreat(&database);
        assert_eq!(tree.focus(), Some(connection_node_id("conn-postgres").as_str()));
        tree.focus_last();
        assert!(tree.focus().is_some());
    }

    #[test]
    fn redis_keyspaces_and_mongo_collections_follow_their_own_models() {
        let mut redis = NavConnection::new("cache", "缓存", Engine::Redis, ConnStatus::Connected);
        redis.default_database = Some("3".to_owned());
        let mut tree = NavTree::new();
        tree.apply(&[redis]);
        tree.expand(&connection_node_id("cache"));
        let rows = tree.visible();
        assert!(rows.iter().all(|row| row.kind != NavKind::Database && row.kind != NavKind::Schema));
        let keyspace = rows.iter().find(|row| row.kind == NavKind::Keyspace).expect("键空间节点");
        assert_eq!(keyspace.label, "db3", "键空间用会话实际库号，不凭空列 16 个");
        assert_eq!(
            tree.activate(&keyspace.id),
            NavAction::OpenKeyspace {
                connection_id: "cache".to_owned(),
                keyspace: "3".to_owned(),
            }
        );

        let mongo = connection(
            Engine::Mongodb,
            vec![database(
                "app",
                vec![schema("", vec![ObjectRow::fresh("users", ObjectKind::Table)])],
            )],
        );
        let mut tree = NavTree::new();
        tree.apply(&[mongo]);
        tree.expand(&connection_node_id("conn-mongodb"));
        tree.expand(&database_node_id("conn-mongodb", "app"));
        let rows = tree.visible();
        assert!(rows.iter().all(|row| row.kind != NavKind::Group), "集合不套关系型对象种类");
        let collection = rows.iter().find(|row| row.kind == NavKind::Object).expect("集合节点");
        assert!(matches!(tree.activate(&collection.id), NavAction::OpenDocuments(_)));
    }

    #[test]
    fn disconnected_and_failed_connections_do_not_invent_children() {
        let mut offline = NavConnection::new("cache", "缓存", Engine::Postgres, ConnStatus::Disconnected);
        offline.databases = vec![database("shop", vec![schema("public", vec![table("orders")])])];
        let mut tree = NavTree::new();
        tree.apply(&[offline]);
        tree.expand(&connection_node_id("cache"));
        assert_eq!(tree.visible().len(), 1, "断开时只显示连接本身");

        let mut failed = NavConnection::new("pg", "销售库", Engine::Postgres, ConnStatus::Connected);
        failed.error = Some("密码错误".to_owned());
        let mut tree = NavTree::new();
        tree.apply(&[failed]);
        assert_eq!(tree.node(&connection_node_id("pg")).unwrap().error.as_deref(), Some("密码错误"));
        assert_eq!(tree.visible().len(), 1);
    }
}
