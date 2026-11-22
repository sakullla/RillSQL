//! MySQL 家族的表结构读回：字段（含 ENUM/SET 候选值、生成列表达式与存储方式）与分区方案。
//!
//! 这些读取只负责把服务器元数据变成结构化快照，不判断业务规则；能力与版本差异仍在
//! 方言档案（super::profile）与各方言之间判定。读失败时保留原因，不把整表当成没读到。

use crate::ddl::model::{
    Column, EnumSetState, GenerationState, GeneratedStorage, LiteralQuoting, PartitionDefState, PartitionMethod, PartitionState, Snapshot,
};

use super::literal;

/// information_schema.columns 的字段查询。
pub(super) const COLUMNS_SQL: &str = "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, GENERATION_EXPRESSION FROM information_schema.columns WHERE table_schema = ? AND table_name = ? ORDER BY ORDINAL_POSITION";

/// information_schema.partitions 的分区查询：只取命名分区，按定义顺序。
pub(super) const PARTITIONS_SQL: &str = "SELECT PARTITION_NAME, PARTITION_METHOD, PARTITION_EXPRESSION, PARTITION_DESCRIPTION FROM information_schema.PARTITIONS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND PARTITION_NAME IS NOT NULL ORDER BY PARTITION_ORDINAL_POSITION";

/// 一个字段读回的结果：列本身加上专有形态。专有形态与列分开保存，普通字段不会多出任何内容。
#[derive(Debug)]
pub(super) struct ColumnRead {
    pub column: Column,
    pub enum_set: Option<EnumSetState>,
    pub generation: Option<GenerationState>,
}

/// 读一列：类型原文、可空、默认值、自增、生成列与 ENUM/SET 候选值。
/// at 是这一组列在行里的起始下标（工作表从 0 开始，批量读取前面还有 TABLE_NAME）。
pub(super) fn column(quoting: LiteralQuoting, row: &mysql_async::Row, at: usize) -> ColumnRead {
    let type_name = super::ddl_read::mysql_string(row, at + 1);
    let extra = super::ddl_read::mysql_string(row, at + 4);
    let expression = super::ddl_read::mysql_string(row, at + 5);
    let generation = generation_of(&extra, &expression);
    let generated = generation.is_some() || super::grid_read::extra_marks_generated(&extra.to_ascii_lowercase());
    let raw_default = match row.as_ref(at + 3) {
        None | Some(mysql_async::Value::NULL) => None,
        Some(_) => Some(super::ddl_read::mysql_string(row, at + 3)),
    };
    ColumnRead {
        column: Column {
            name: super::ddl_read::mysql_string(row, at),
            type_name: type_name.clone(),
            nullable: super::ddl_read::mysql_string(row, at + 2).eq_ignore_ascii_case("YES"),
            // SQL NULL 是无默认值；空串是 DEFAULT ''。表达式与含引号文本的规整见 reported_default。
            default_sql: super::types::reported_default(raw_default.as_deref(), &extra.to_ascii_lowercase()),
            primary: false,
            auto: extra.to_ascii_lowercase().contains("auto_increment"),
            generated,
            default_constraint: None,
        },
        enum_set: literal::parse_enum_set(&type_name, quoting),
        generation,
    }
}

/// EXTRA 与 GENERATION_EXPRESSION 转成生成列状态。只有真正的生成列才返回 Some：
/// MySQL 8 给表达式默认值标的 DEFAULT_GENERATED 不是生成列。
pub(super) fn generation_of(extra: &str, expression: &str) -> Option<GenerationState> {
    let extra = extra.to_ascii_lowercase();
    if expression.is_empty() && !super::grid_read::extra_marks_generated(&extra) {
        return None;
    }
    let (storage, reported) = if extra.contains("persistent generated") {
        (GeneratedStorage::Stored, "PERSISTENT")
    } else if extra.contains("stored generated") {
        (GeneratedStorage::Stored, "STORED")
    } else {
        (GeneratedStorage::Virtual, "VIRTUAL")
    };
    Some(GenerationState { expression: expression.to_owned(), storage, reported_storage: reported.to_owned() })
}

/// 分区行（名字、方式、表达式、边界原文）转成分区方案。没有命名分区（表未分区）时返回 None。
pub(super) fn partition_state(rows: &[(String, String, String, String)]) -> Option<PartitionState> {
    let first = rows.first()?;
    let method = match first.1.trim().to_ascii_uppercase().as_str() {
        "RANGE" => PartitionMethod::Range,
        "LIST" => PartitionMethod::List,
        "HASH" => PartitionMethod::Hash,
        "KEY" => PartitionMethod::Key,
        // 读回的方式不在本轮范围内时（如 SYSTEM）不猜，交给调用方说明读到的原文。
        _ => return None,
    };
    let definitions = rows
        .iter()
        .map(|(name, _, _, description)| PartitionDefState { name: name.clone(), description: description.clone() })
        .collect();
    Some(PartitionState { method, expression: first.2.clone(), definitions })
}

/// KEY 分区的列清单还原：information_schema.PARTITIONS.PARTITION_EXPRESSION 对 KEY 分区返回的
/// 是反引号包住的列清单（MySQL 与 MariaDB 都返回如 `id`,`tenant_id`），不是裸列名。
/// 这里按逗号拆开、去空白、去掉外层反引号并把连续两个反引号还原成一个，界面起稿与重建都拿裸列名；
/// 再加引号由 table_ddl::key_columns 按方言做，避免出现 KEY(```id```) 这种再包一层的写法。
/// 逗号只在反引号之外才算分隔，列名里的逗号不会被拆开。RANGE/LIST/HASH 的表达式不是列清单，不走这里。
pub(crate) fn key_columns_from_expression(expression: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    for ch in expression.chars() {
        match ch {
            '`' => {
                quoted = !quoted;
                token.push(ch);
            }
            ',' if !quoted => tokens.push(std::mem::take(&mut token)),
            _ => token.push(ch),
        }
    }
    tokens.push(token);
    tokens.iter().filter_map(|token| unquote_column(token)).collect()
}

/// 一个列清单片段还原成裸列名：去空白；成对的反引号去掉并把 `` 还原成 `；空的片段丢掉。
fn unquote_column(token: &str) -> Option<String> {
    let text = token.trim();
    let name = match text.strip_prefix('`').and_then(|rest| rest.strip_suffix('`')) {
        Some(inner) => inner.replace("``", "`"),
        None => text.to_owned(),
    };
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// 读一张表的分区方案。查询失败时给出原因，由调用方记进 snapshot.partition_error，不影响字段读取。
pub(super) async fn load_partitions(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<Option<PartitionState>, String> {
    let rows: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(conn, PARTITIONS_SQL, (database.to_owned(), table.to_owned()))
        .await
        .map_err(|err| format!("读不到分区信息：{}", crate::error_text::chain(&err)))?;
    let tuples: Vec<(String, String, String, String)> = rows
        .iter()
        .map(|row| {
            (
                super::ddl_read::mysql_string(row, 0),
                super::ddl_read::mysql_string(row, 1),
                super::ddl_read::mysql_string(row, 2),
                super::ddl_read::mysql_string(row, 3),
            )
        })
        .collect();
    Ok(partition_state(&tuples))
}

/// 把读回结果并入快照：列、ENUM/SET 候选值与生成列信息。
pub(super) fn push_column(snapshot: &mut Snapshot, row: &mysql_async::Row, at: usize) {
    let quoted = snapshot.literal_quoting();
    let read = column(quoted, row, at);
    if let Some(enum_set) = read.enum_set {
        snapshot.enum_sets.insert(read.column.name.clone(), enum_set);
    }
    if let Some(generation) = read.generation {
        snapshot.generation.insert(read.column.name.clone(), generation);
    }
    snapshot.columns.push(read.column);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有真正的生成列才带表达式与存储方式；DEFAULT_GENERATED 是表达式默认值，不是生成列。
    #[test]
    fn generation_state_distinguishes_generated_columns_from_expression_defaults() {
        let virtual_column = generation_of("virtual generated", "price * quantity").expect("virtual");
        assert_eq!(virtual_column.storage, GeneratedStorage::Virtual);
        assert_eq!(virtual_column.reported_storage, "VIRTUAL");
        assert_eq!(virtual_column.expression, "price * quantity");

        let stored = generation_of("stored generated", "price * 2").expect("stored");
        assert_eq!(stored.storage, GeneratedStorage::Stored);
        assert_eq!(stored.reported_storage, "STORED");

        // MariaDB 的持久化写法读回是 PERSISTENT，语义等同于 STORED，原文保留用于展示。
        let persistent = generation_of("persistent generated", "price + 1").expect("persistent");
        assert_eq!(persistent.storage, GeneratedStorage::Stored);
        assert_eq!(persistent.reported_storage, "PERSISTENT");

        assert!(generation_of("default_generated", "").is_none());
        assert!(generation_of("DEFAULT_GENERATED on update CURRENT_TIMESTAMP", "").is_none());
        assert!(generation_of("", "").is_none());
    }

    /// 四种分区方式都读得出来，分区顺序与边界原文保持不变；未分区与不认识的写法不猜。
    #[test]
    fn partition_rows_keep_method_expression_and_defined_order() {
        let range = partition_state(&[
            ("p0".to_owned(), "RANGE".to_owned(), "created".to_owned(), "100".to_owned()),
            ("p1".to_owned(), "RANGE".to_owned(), "created".to_owned(), "MAXVALUE".to_owned()),
        ])
        .expect("range");
        assert_eq!(range.method, PartitionMethod::Range);
        assert_eq!(range.expression, "created");
        assert_eq!(range.definitions.iter().map(|def| def.name.as_str()).collect::<Vec<_>>(), vec!["p0", "p1"]);
        assert_eq!(range.definitions[1].description, "MAXVALUE");

        let list = partition_state(&[("p_east".to_owned(), "LIST".to_owned(), "region".to_owned(), "1,2".to_owned())]).expect("list");
        assert_eq!(list.method, PartitionMethod::List);
        assert_eq!(list.definitions[0].description, "1,2");

        let hash = partition_state(&[("p0".to_owned(), "HASH".to_owned(), "id".to_owned(), String::new())]).expect("hash");
        assert_eq!(hash.method, PartitionMethod::Hash);
        let key = partition_state(&[("p0".to_owned(), "KEY".to_owned(), "id".to_owned(), String::new())]).expect("key");
        assert_eq!(key.method, PartitionMethod::Key);

        assert!(partition_state(&[]).is_none());
        assert!(partition_state(&[(String::new(), "SYSTEM".to_owned(), String::new(), String::new())]).is_none());
    }

    /// KEY 分区的读回表达式是反引号包住的列清单：还原成裸列名，才不会再包一层引号。
    #[test]
    fn key_columns_from_read_back_expression_are_bare_names() {
        // 两个引擎读回的实际形状。
        assert_eq!(key_columns_from_expression("`id`,`tenant_id`"), vec!["id".to_owned(), "tenant_id".to_owned()]);
        // 空格不影响结果，空片段丢掉。
        assert_eq!(key_columns_from_expression(" `id` , `tenant_id` , "), vec!["id".to_owned(), "tenant_id".to_owned()]);
        // 列名里的反引号在表达式里是双写的：还原成一个。
        assert_eq!(key_columns_from_expression("`a``b`"), vec!["a`b".to_owned()]);
        assert_eq!(
            key_columns_from_expression("`a``b`,`c`"),
            vec!["a`b".to_owned(), "c".to_owned()]
        );
        // 反引号里的逗号是列名的一部分，不当分隔符。
        assert_eq!(key_columns_from_expression("`a,b`"), vec!["a,b".to_owned()]);
        // 没有反引号的写法（用户在界面上手写的裸列名）原样保留。
        assert_eq!(key_columns_from_expression("id, tenant_id"), vec!["id".to_owned(), "tenant_id".to_owned()]);
        assert!(key_columns_from_expression("   ").is_empty());
        assert!(key_columns_from_expression("").is_empty());
    }
}
