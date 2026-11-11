//! 对象树“查看/复制 DDL”的定义来源。
//!
//! 优先给数据库保存的原文；PostgreSQL 与 SQL Server 的表没有原文，按元数据重建并标明缺了什么；
//! 读不到定义的种类由 [`ddl_available`] 提前判定，菜单据此禁用，不打开空的查看器。

use crate::connections::CatalogSession;

use super::model::{kind_name, Engine, ObjectKind};
use super::read::read_definition;

/// 定位一个对象所需的字段。和对象树节点上的字段一一对应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DdlKey {
    pub database: String,
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    /// PostgreSQL 重载函数的参数签名；其余为空。
    pub signature: String,
    /// 触发器所属的表；其余为空。
    pub relation: String,
}

/// 读到的定义。`rebuilt` 为 true 时文本是按元数据重建的，`missing` 列出没有包含的内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectDdl {
    pub text: String,
    pub rebuilt: bool,
    pub missing: Vec<String>,
}

/// 这个引擎的这种对象能不能读到定义。纯函数：菜单用它决定“查看 DDL”是否可点。
pub fn ddl_available(engine: Engine, kind: ObjectKind) -> bool {
    use ObjectKind::*;
    match engine {
        Engine::Sqlite => matches!(kind, Table | View | Trigger),
        Engine::Mysql => matches!(kind, Table | View | Function | Procedure | Trigger | Event),
        Engine::Mariadb => matches!(kind, Table | View | Function | Procedure | Trigger | Event | Sequence),
        Engine::Postgres => matches!(kind, Table | View | Function | Procedure | Trigger | Sequence | Enum | Domain),
        Engine::Sqlserver => matches!(kind, Table | View | Function | Procedure | Trigger | Sequence),
        Engine::Redis | Engine::Mongodb => false,
    }
}

/// 这个引擎的表定义是不是按元数据重建的（没有数据库保存的建表原文）。
pub fn table_is_rebuilt(engine: Engine) -> bool {
    matches!(engine, Engine::Postgres | Engine::Sqlserver)
}

/// 读对象定义。不可用的种类直接返回原因，不发查询。
pub async fn object_ddl(session: &CatalogSession, engine: Engine, key: &DdlKey) -> Result<ObjectDdl, String> {
    if !ddl_available(engine, key.kind) {
        return Err(format!("读取定义失败：{} 的{}没有可读的定义", engine.as_str(), kind_name(key.kind)));
    }
    if key.kind == ObjectKind::Table && table_is_rebuilt(engine) {
        return rebuilt_table(session, engine, key).await;
    }
    let text = read_definition(session, engine, &key.database, &key.schema, &key.name, kind_name(key.kind), &key.signature, &key.relation).await?;
    Ok(ObjectDdl { text, rebuilt: false, missing: Vec::new() })
}

async fn rebuilt_table(session: &CatalogSession, engine: Engine, key: &DdlKey) -> Result<ObjectDdl, String> {
    let request = crate::ddl::Request {
        database: key.database.clone(),
        schema: key.schema.clone(),
        engine,
        action: crate::ddl::Action::DropTable { table: key.name.clone() },
    };
    let mut snapshot = crate::ddl::load_partial(session, &request, &key.name)
        .await
        .map_err(|message| format!("读取定义失败：{message}"))?;
    if snapshot.kind != "table" {
        return Err(format!("读取定义失败：{} 不是基础表", key.name));
    }
    crate::ddl::load_comments(session, &mut snapshot).await;
    Ok(rebuilt_from_snapshot(&snapshot))
}

/// 由已读到的结构生成重建定义。单独拆出来，便于不连数据库验证文本。
pub fn rebuilt_from_snapshot(snapshot: &crate::ddl::Snapshot) -> ObjectDdl {
    let (text, missing) = crate::ddl::rebuilt_definition(snapshot);
    ObjectDdl { text, rebuilt: true, missing }
}

/// 读取错误里的通用前缀；查看器与状态栏自己已经写明“读取定义失败”，显示时去掉，避免重复。
const FAILED_PREFIX: &str = "读取定义失败";

/// 查看器与状态栏显示的读取错误：去掉重复的“读取定义失败：”前缀，再按界面语言给出（数据库原文不翻译）。
/// 只在显示时调用，模型里保留原文，切换语言立即生效。
pub fn error_text(message: &str, locale: crate::i18n::Locale) -> String {
    let mut rest = message.trim();
    while let Some(after) = rest.strip_prefix(FAILED_PREFIX) {
        rest = after.strip_prefix('：').unwrap_or(after).trim_start();
    }
    crate::ddl::english::localize(rest, locale)
}

/// 重建定义“未包含”清单按界面语言逐项给出；生成 SQL 开头注释里的同一份清单不改。
pub fn missing_text(missing: &[String], locale: crate::i18n::Locale) -> Vec<String> {
    missing.iter().map(|item| crate::ddl::english::localize_missing_item(item, locale)).collect()
}
