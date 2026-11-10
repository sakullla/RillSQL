//! 对象树的纯规则：节点身份与对象动作规格、引擎相关的限定名、插入文本。
//! 这里不读写 GPUI，模型测试可以直接调用。

use crate::app::model::ObjectTarget;
use crate::catalog::hierarchy::{NavNode, ObjectKey};
use crate::catalog::model::Engine;
use crate::query::assist::quote_ident;

/// 节点 -> 完整对象目标。环境只用于展示，不参与身份。
pub fn target_for(node: &NavNode, environment: &str) -> Option<ObjectTarget> {
    let object = node.object()?;
    Some(ObjectTarget::new(
        object.connection_id,
        object.connection_name,
        object.database,
        object.schema,
        object.name,
        object.engine.as_str(),
        environment,
    ))
}

/// 树里某一行的对象规格；菜单动作、快速打开与工作项读同一份。
pub fn spec_for(node: &NavNode, environment: &str) -> Option<String> {
    target_for(node, environment).map(|target| target.spec())
}

/// 复制用的限定名：按引擎层级写，不带连接展示名。
pub fn qualified_name(object: &ObjectKey) -> String {
    object.qualified_name()
}

fn quoted(engine: Engine, database: &str, schema: &str, name: &str) -> String {
    match engine {
        Engine::Sqlite => quote_ident("sqlite", name),
        Engine::Mysql | Engine::Mariadb => {
            if database.is_empty() {
                quote_ident("mysql", name)
            } else {
                format!("{}.{}", quote_ident("mysql", database), quote_ident("mysql", name))
            }
        }
        engine => {
            if schema.is_empty() {
                quote_ident(engine.as_str(), name)
            } else {
                format!("{}.{}", quote_ident(engine.as_str(), schema), quote_ident(engine.as_str(), name))
            }
        }
    }
}

/// 对象名插入文本：同一引擎的引用规则。
pub fn insert_object_text(object: &ObjectKey) -> String {
    quoted(object.engine, &object.database, &object.schema, &object.name)
}

/// 复制/插入的列名。列默认不带表前缀，插入点由用户决定。
pub fn insert_column_text(column: &str) -> String {
    column.to_owned()
}

/// 生成 SELECT 的插入文本：只写草稿，永不执行。
pub fn select_text(object: &ObjectKey) -> String {
    let name = quoted(object.engine, &object.database, &object.schema, &object.name);
    format!("SELECT *\nFROM {name}\nLIMIT 100;")
}

/// 对象树“生成 INSERT / UPDATE / DELETE 模板”的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateKind {
    Insert,
    Update,
    Delete,
}

impl TemplateKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "insert" => Some(Self::Insert),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

fn quoted_column(engine: Engine, column: &str) -> String {
    quote_ident(engine.as_str(), column)
}

/// SQL 模板：表名与列名按引擎引用，值用 `?` 占位。只写草稿，永不执行。
/// UPDATE/DELETE 按主键写 WHERE；没有主键时留一个条件注释，不替用户猜条件。
pub fn template_text(kind: TemplateKind, object: &ObjectKey, columns: &[(String, bool)]) -> String {
    let table = quoted(object.engine, &object.database, &object.schema, &object.name);
    let names: Vec<String> = columns.iter().map(|(name, _)| quoted_column(object.engine, name)).collect();
    let keys: Vec<String> = columns
        .iter()
        .filter(|(_, primary)| *primary)
        .map(|(name, _)| quoted_column(object.engine, name))
        .collect();
    let condition = if keys.is_empty() {
        "/* condition */".to_owned()
    } else {
        keys.iter().map(|key| format!("{key} = ?")).collect::<Vec<_>>().join("\n  AND ")
    };
    match kind {
        TemplateKind::Insert => {
            let values = vec!["?"; names.len()].join(", ");
            format!("INSERT INTO {table} ({})\nVALUES ({values});", names.join(", "))
        }
        TemplateKind::Update => {
            // 主键只进 WHERE；全是主键的表仍给出全部列，模板不能没有 SET。
            let set: Vec<&String> = names
                .iter()
                .zip(columns)
                .filter(|(_, (_, primary))| !primary)
                .map(|(name, _)| name)
                .collect();
            let set: Vec<&String> = if set.is_empty() { names.iter().collect() } else { set };
            let assignments = set.iter().map(|name| format!("{name} = ?")).collect::<Vec<_>>().join(",\n    ");
            format!("UPDATE {table}\nSET {assignments}\nWHERE {condition};")
        }
        TemplateKind::Delete => format!("DELETE FROM {table}\nWHERE {condition};"),
    }
}

/// 把一段文本并入当前草稿：非空草稿之间留一个换行，不碰已有内容。
pub fn append_text(draft: &str, text: &str) -> String {
    if text.is_empty() {
        return draft.to_owned();
    }
    if draft.is_empty() {
        return text.to_owned();
    }
    if draft.ends_with(char::is_whitespace) {
        format!("{draft}{text}")
    } else {
        format!("{draft}\n{text}")
    }
}

/// Redis 的键浏览规格：连接|名称|库号。
pub fn redis_spec(connection_id: &str, connection_name: &str, keyspace: &str) -> String {
    format!("{connection_id}|{connection_name}|{keyspace}")
}

/// MongoDB 的文档浏览规格：连接|名称|库|集合。
pub fn mongo_spec(connection_id: &str, connection_name: &str, database: &str, collection: &str) -> String {
    format!("{connection_id}|{connection_name}|{database}|{collection}")
}

/// 外壳需要异步完成的一次目录读取。模型只排队，由视图负责发起。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavRequest {
    /// 整个目录或某一条连接重读。
    Refresh { connection: Option<String> },
    /// 按需读取一个对象的列与索引。
    Details(String),
    /// 继续读取一个库/模式范围的对象。
    More(String),
    /// 只重读一个对象种类分组（局部失败后的重试，或“刷新此节点”）。
    Group(String),
    /// 从第一页重新读一个库/模式范围（“刷新此节点”）；展开状态保留。
    Reload(String),
    /// 读一个对象的定义（查看/复制 DDL）。
    Definition(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::model::ObjectKind;

    fn key(engine: Engine, database: &str, schema: &str, name: &str) -> ObjectKey {
        ObjectKey {
            connection_id: "c1".to_owned(),
            connection_name: "销售库".to_owned(),
            engine,
            database: database.to_owned(),
            schema: schema.to_owned(),
            name: name.to_owned(),
            kind: ObjectKind::Table,
        }
    }

    #[test]
    fn qualified_names_follow_each_engine_level() {
        assert_eq!(qualified_name(&key(Engine::Postgres, "shop", "public", "orders")), "public.orders");
        assert_eq!(qualified_name(&key(Engine::Mysql, "shop", "", "orders")), "shop.orders");
        assert_eq!(qualified_name(&key(Engine::Sqlite, "main", "", "orders")), "orders");
    }

    #[test]
    fn insert_texts_quote_identifiers_and_never_run() {
        assert_eq!(insert_object_text(&key(Engine::Postgres, "shop", "public", "orders")), "\"public\".\"orders\"");
        assert_eq!(
            insert_object_text(&key(Engine::Postgres, "shop", "public", "or\"ders")),
            "\"public\".\"or\"\"ders\""
        );
        let sql = select_text(&key(Engine::Postgres, "shop", "public", "orders"));
        assert!(sql.starts_with("SELECT *"));
        assert!(sql.contains("FROM \"public\".\"orders\""));
        assert!(sql.ends_with(';'));
    }

    #[test]
    fn appending_keeps_existing_draft_and_adds_one_line_break() {
        assert_eq!(append_text("", "abc"), "abc");
        assert_eq!(append_text("select 1", "abc"), "select 1\nabc");
        assert_eq!(append_text("select 1\n", "abc"), "select 1\nabc");
        assert_eq!(append_text("select 1  ", "abc"), "select 1  abc");
        assert_eq!(append_text("select 1", ""), "select 1");
    }

    #[test]
    fn templates_quote_names_follow_primary_keys_and_never_run() {
        let columns = vec![("id".to_owned(), true), ("label".to_owned(), false), ("note".to_owned(), false)];
        let pg = key(Engine::Postgres, "shop", "public", "orders");
        assert_eq!(
            template_text(TemplateKind::Insert, &pg, &columns),
            "INSERT INTO \"public\".\"orders\" (\"id\", \"label\", \"note\")\nVALUES (?, ?, ?);"
        );
        assert_eq!(
            template_text(TemplateKind::Update, &pg, &columns),
            "UPDATE \"public\".\"orders\"\nSET \"label\" = ?,\n    \"note\" = ?\nWHERE \"id\" = ?;"
        );
        assert_eq!(template_text(TemplateKind::Delete, &pg, &columns), "DELETE FROM \"public\".\"orders\"\nWHERE \"id\" = ?;");

        let mysql = key(Engine::Mysql, "shop", "", "orders");
        let update = template_text(TemplateKind::Update, &mysql, &columns);
        assert!(update.starts_with("UPDATE `shop`.`orders`"), "{update}");
        assert!(update.contains("`label` = ?"), "{update}");
        let mariadb = key(Engine::Mariadb, "shop", "", "orders");
        assert!(template_text(TemplateKind::Delete, &mariadb, &columns).contains("`id` = ?"));

        // 没有主键：条件留给用户，不替他猜。
        let loose = vec![("a".to_owned(), false), ("b".to_owned(), false)];
        let sqlite = key(Engine::Sqlite, "main", "", "notes");
        assert_eq!(template_text(TemplateKind::Delete, &sqlite, &loose), "DELETE FROM \"notes\"\nWHERE /* condition */;");
        let update = template_text(TemplateKind::Update, &sqlite, &loose);
        assert!(update.contains("\"a\" = ?") && update.contains("\"b\" = ?") && update.ends_with("WHERE /* condition */;"), "{update}");
        let sqlserver = key(Engine::Sqlserver, "shop", "dbo", "orders");
        assert!(template_text(TemplateKind::Insert, &sqlserver, &columns).starts_with("INSERT INTO [dbo].[orders] ([id], [label], [note])"));
        assert_eq!(TemplateKind::parse("update"), Some(TemplateKind::Update));
        assert_eq!(TemplateKind::parse(TemplateKind::Delete.as_str()), Some(TemplateKind::Delete));
        assert_eq!(TemplateKind::parse("merge"), None);
    }

    #[test]
    fn redis_and_mongo_specs_keep_engine_specific_shape() {
        assert_eq!(redis_spec("cache", "缓存", "3"), "cache|缓存|3");
        assert_eq!(mongo_spec("docs", "文档", "app", "users"), "docs|文档|app|users");
    }
}
