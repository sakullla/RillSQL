//! MySQL 家族的表的专有结构：ENUM/SET 类型、生成列子句与分区方案的语句生成。
//!
//! 这里只做“草稿变成语句”，不执行也不读回：执行仍由既有的确认执行链完成，
//! 读回在 super::table_read。哪些操作适用于哪种分区方式在这里判断，不适用的只给阻断原因。

use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::ddl::model::{
    EnumSetDraft, GeneratedColumnDraft, GeneratedStorage, LiteralQuoting, PartitionDef, PartitionDraft, PartitionMethod, PartitionOp, Plan, Risk, Snapshot, Step,
};
use crate::ddl::plan::{finish, target_label};
use crate::ddl::sql::{qualified_table, valid_name};

use super::literal;

/// ENUM/SET 的类型写法。候选值经统一的字面量编码器转义，界面里的输入不再二次转义。
pub(super) fn enum_set_type(draft: &EnumSetDraft, quoting: LiteralQuoting) -> Result<String, String> {
    literal::enum_set_type(draft.kind, &draft.values, quoting)
}

/// 生成列子句：GENERATED ALWAYS AS (表达式) VIRTUAL|STORED|PERSISTENT。
/// stored_keyword 取方言档案里的写法（MariaDB 用 PERSISTENT），不假设两种写法在两边都能用。
pub(super) fn generated_clause(draft: &GeneratedColumnDraft, stored_keyword: &str) -> Result<String, String> {
    let expression = draft.expression.trim();
    if expression.is_empty() {
        return Err("生成列的表达式为空。".to_owned());
    }
    let keyword = match draft.storage {
        GeneratedStorage::Virtual => "VIRTUAL",
        GeneratedStorage::Stored => stored_keyword,
    };
    Ok(format!(" GENERATED ALWAYS AS ({expression}) {keyword}"))
}

/// PARTITION BY 子句（用于重建整张表的分区方案）。
pub(super) fn partition_by_clause(draft: &PartitionDraft) -> Result<String, String> {
    if let Some(problem) = partition_problem(draft) {
        return Err(problem);
    }
    let expression = draft.expression.trim();
    let body = match draft.method {
        PartitionMethod::Range | PartitionMethod::List => {
            format!("{}({}) ({})", draft.method.keyword(), expression, definitions_sql(draft.method, &draft.definitions)?)
        }
        PartitionMethod::Hash => format!("{}({}) PARTITIONS {}", draft.method.keyword(), expression, count_of(draft)?),
        PartitionMethod::Key => format!("{}({}) PARTITIONS {}", draft.method.keyword(), key_columns(draft)?, count_of(draft)?),
    };
    Ok(format!("PARTITION BY {body}"))
}

/// 一个分区定义：RANGE 是 VALUES LESS THAN (...)，LIST 是 VALUES IN (...)。
fn definition_sql(method: PartitionMethod, def: &PartitionDef) -> Result<String, String> {
    let name = def.name.trim();
    if !valid_name(name) {
        return Err("分区名无效。".to_owned());
    }
    let description = def.description.trim();
    if description.is_empty() {
        return Err(format!("分区 {name} 没有边界值。"));
    }
    // 边界值原样写进括号：用户输入的是 SQL 里的值列表（如 100、'2026-01-01'、MAXVALUE）。
    let keyword = if method == PartitionMethod::Range { "VALUES LESS THAN" } else { "VALUES IN" };
    Ok(format!("PARTITION {} {keyword} ({description})", quote_ident(Engine::Mysql, name)))
}

fn definitions_sql(method: PartitionMethod, definitions: &[PartitionDef]) -> Result<String, String> {
    if definitions.is_empty() {
        return Err("没有分区定义。".to_owned());
    }
    let mut rendered = Vec::with_capacity(definitions.len());
    for def in definitions {
        rendered.push(definition_sql(method, def)?);
    }
    Ok(rendered.join(", "))
}

fn key_columns(draft: &PartitionDraft) -> Result<String, String> {
    let names: Vec<String> = draft.columns.iter().map(|name| name.trim().to_owned()).filter(|name| !name.is_empty()).collect();
    if names.is_empty() {
        return Err("KEY 分区要至少一个列。".to_owned());
    }
    Ok(names.iter().map(|name| quote_ident(Engine::Mysql, name)).collect::<Vec<_>>().join(", "))
}

fn count_of(draft: &PartitionDraft) -> Result<u32, String> {
    let text = draft.count.trim();
    let count: u32 = text.parse().map_err(|_| format!("分区数量 {text} 不是正整数。"))?;
    if count == 0 {
        return Err("分区数量要大于 0。".to_owned());
    }
    Ok(count)
}

/// 草稿本身的问题：方式与操作不匹配、缺表达式或缺定义。客户端能判断的在这里阻断。
pub(super) fn partition_problem(draft: &PartitionDraft) -> Option<String> {
    match draft.op {
        PartitionOp::Replace => {
            if draft.method.named() && draft.definitions.is_empty() {
                return Some("没有分区定义。".to_owned());
            }
            if draft.method == PartitionMethod::Hash && draft.expression.trim().is_empty() {
                return Some("HASH 分区要一个表达式。".to_owned());
            }
            if draft.method == PartitionMethod::Key && draft.columns.iter().all(|name| name.trim().is_empty()) {
                return Some("KEY 分区要至少一个列。".to_owned());
            }
        }
        PartitionOp::Add => {
            if draft.method.named() && draft.definitions.is_empty() {
                return Some("没有要新增的分区定义。".to_owned());
            }
        }
        PartitionOp::Drop => {
            if draft.method.named() && draft.targets.is_empty() {
                return Some("没有要删除的分区。".to_owned());
            }
            if !draft.method.named() && draft.count.trim().is_empty() {
                return Some("HASH/KEY 分区要写明减少的数量。".to_owned());
            }
        }
        PartitionOp::Reorganize => {
            if draft.targets.is_empty() {
                return Some("没有指定要重组的分区。".to_owned());
            }
            if draft.definitions.is_empty() {
                return Some("没有新的分区定义。".to_owned());
            }
        }
        PartitionOp::Remove => {}
    }
    if draft.method.named() && matches!(draft.op, PartitionOp::Replace | PartitionOp::Add | PartitionOp::Reorganize) && draft.expression.trim().is_empty() {
        return Some(format!("{} 分区要一个表达式或列。", draft.method.keyword()));
    }
    None
}

/// 分区变更的语句。不适用时返回原因，不生成半条语句。
pub(super) fn partition_steps(snapshot: &Snapshot, table: &str, draft: &PartitionDraft) -> Result<Vec<Step>, String> {
    let qualified = qualified_table(Engine::Mysql, &snapshot.database, &snapshot.schema, table)?;
    let sql = match draft.op {
        PartitionOp::Replace => format!("ALTER TABLE {qualified} {};", partition_by_clause(draft)?),
        PartitionOp::Add => match draft.method {
            PartitionMethod::Range | PartitionMethod::List => {
                format!("ALTER TABLE {qualified} ADD PARTITION ({});", definitions_sql(draft.method, &draft.definitions)?)
            }
            // HASH/KEY 只能按数量增删，单个分区没有意义。
            PartitionMethod::Hash | PartitionMethod::Key => format!("ALTER TABLE {qualified} ADD PARTITION PARTITIONS {};", count_of(draft)?),
        },
        PartitionOp::Drop => match draft.method {
            PartitionMethod::Range | PartitionMethod::List => {
                let names: Vec<String> = draft.targets.iter().map(|name| quote_ident(Engine::Mysql, name.trim())).collect();
                if names.is_empty() {
                    return Err("没有要删除的分区。".to_owned());
                }
                format!("ALTER TABLE {qualified} DROP PARTITION {};", names.join(", "))
            }
            PartitionMethod::Hash | PartitionMethod::Key => format!("ALTER TABLE {qualified} COALESCE PARTITION {};", count_of(draft)?),
        },
        PartitionOp::Reorganize => {
            let names: Vec<String> = draft.targets.iter().map(|name| quote_ident(Engine::Mysql, name.trim())).collect();
            format!(
                "ALTER TABLE {qualified} REORGANIZE PARTITION {} INTO ({});",
                names.join(", "),
                definitions_sql(draft.method, &draft.definitions)?
            )
        }
        PartitionOp::Remove => format!("ALTER TABLE {qualified} REMOVE PARTITIONING;"),
    };
    let label = match draft.op {
        PartitionOp::Replace => format!("重建 {table} 的分区方案"),
        PartitionOp::Add => format!("给 {table} 新增分区"),
        PartitionOp::Drop => format!("删除 {table} 的分区"),
        PartitionOp::Reorganize => format!("重组 {table} 的分区"),
        PartitionOp::Remove => format!("移除 {table} 的分区方案"),
    };
    Ok(vec![Step { sql, label }])
}

/// 分区变更的实际后果：换方案与重组会重写数据，删除分区会连同分区里的行一起丢掉。
pub(super) fn partition_risks(draft: &PartitionDraft) -> Vec<Risk> {
    let mut risks = Vec::new();
    match draft.op {
        PartitionOp::Replace => risks.push(Risk {
            level: "danger".to_owned(),
            text: "重建分区方案会按新规则重新分布已有数据，期间会大量重写，可能长时间占用服务器。".to_owned(),
        }),
        PartitionOp::Drop => risks.push(Risk {
            level: "danger".to_owned(),
            text: "删除分区会同时删除该分区里的全部行，不能回滚。".to_owned(),
        }),
        PartitionOp::Reorganize => risks.push(Risk {
            level: "warn".to_owned(),
            text: "重组分区会把受影响分区的行重新分布到新分区，需要重写这些数据。".to_owned(),
        }),
        PartitionOp::Remove => risks.push(Risk {
            level: "warn".to_owned(),
            text: "移除分区方案会把所有分区的数据合并回一张未分区表，需要重写整表。".to_owned(),
        }),
        PartitionOp::Add => risks.push(Risk { level: "info".to_owned(), text: "新增分区不改已有行。".to_owned() }),
    }
    risks
}

/// 分区动作的计划。supported 为假时是别的引擎请求了它：给出同一动作的拒绝计划。
pub(super) fn plan_partition(snapshot: &Snapshot, table: &str, draft: &PartitionDraft, supported: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    let risks = partition_risks(draft);
    if !supported {
        return finish(snapshot.engine, false, target, vec!["只有 MySQL 与 MariaDB 支持这里的分区变更。".to_owned()], risks, Vec::new());
    }
    let mut blocked = Vec::new();
    if snapshot.kind != "table" {
        blocked.push(format!("{table} 不是表，不能改分区。"));
    }
    if !valid_name(table) {
        blocked.push("表名无效。".to_owned());
    }
    if let Some(problem) = partition_problem(draft) {
        blocked.push(problem);
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match partition_steps(snapshot, table, draft) {
        Ok(steps) => finish(snapshot.engine, true, target, Vec::new(), risks, steps),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(backslash_escapes: bool) -> LiteralQuoting {
        LiteralQuoting { backslash_escapes }
    }

    fn snapshot(name: &str) -> Snapshot {
        Snapshot::bare(Engine::Mysql, "app", "app", name)
    }

    fn def(name: &str, description: &str) -> PartitionDef {
        PartitionDef { name: name.to_owned(), description: description.to_owned() }
    }

    fn draft(method: PartitionMethod, op: PartitionOp) -> PartitionDraft {
        PartitionDraft { op, method, expression: "created".to_owned(), ..PartitionDraft::default() }
    }

    /// ENUM/SET 的类型写法走统一编码器；候选值为空时给出原因。
    #[test]
    fn enum_set_type_uses_the_shared_literal_encoder() {
        let draft = EnumSetDraft { kind: crate::ddl::model::EnumSetKind::Enum, values: vec![String::new(), "it's".to_owned(), "中文".to_owned()] };
        assert_eq!(enum_set_type(&draft, quoted(true)).expect("sql"), "enum('','it''s','中文')");
        let empty = EnumSetDraft { kind: crate::ddl::model::EnumSetKind::Set, values: Vec::new() };
        assert!(enum_set_type(&empty, quoted(true)).is_err());
    }

    /// 生成列的写法按存储方式与厂商关键字：VIRTUAL / STORED / MariaDB 的 PERSISTENT。
    #[test]
    fn generated_clause_follows_the_storage_keyword_of_the_dialect() {
        let virtual_column = GeneratedColumnDraft { expression: "price * quantity".to_owned(), storage: GeneratedStorage::Virtual };
        assert_eq!(generated_clause(&virtual_column, "STORED").expect("sql"), " GENERATED ALWAYS AS (price * quantity) VIRTUAL");
        let stored = GeneratedColumnDraft { expression: "price * 2".to_owned(), storage: GeneratedStorage::Stored };
        assert_eq!(generated_clause(&stored, "STORED").expect("sql"), " GENERATED ALWAYS AS (price * 2) STORED");
        assert_eq!(generated_clause(&stored, "PERSISTENT").expect("sql"), " GENERATED ALWAYS AS (price * 2) PERSISTENT");
        let empty = GeneratedColumnDraft { expression: "  ".to_owned(), storage: GeneratedStorage::Virtual };
        assert!(generated_clause(&empty, "STORED").is_err());
    }

    /// 四种分区方式的语句形状：命名分区带 VALUES LESS THAN / VALUES IN，HASH/KEY 按数量。
    #[test]
    fn partition_statements_cover_the_four_methods() {
        let mut range = draft(PartitionMethod::Range, PartitionOp::Replace);
        range.definitions = vec![def("p0", "100"), def("p1", "MAXVALUE")];
        assert_eq!(
            partition_steps(&snapshot("orders"), "orders", &range).expect("sql")[0].sql,
            "ALTER TABLE app.orders PARTITION BY RANGE(created) (PARTITION p0 VALUES LESS THAN (100), PARTITION p1 VALUES LESS THAN (MAXVALUE));"
        );

        let mut list = draft(PartitionMethod::List, PartitionOp::Replace);
        list.expression = "region".to_owned();
        list.definitions = vec![def("p_east", "1,2")];
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &list).expect("sql")[0].sql,
            "ALTER TABLE app.t PARTITION BY LIST(region) (PARTITION p_east VALUES IN (1,2));"
        );

        let mut hash = draft(PartitionMethod::Hash, PartitionOp::Replace);
        hash.expression = "id".to_owned();
        hash.count = "4".to_owned();
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &hash).expect("sql")[0].sql,
            "ALTER TABLE app.t PARTITION BY HASH(id) PARTITIONS 4;"
        );

        let mut key = draft(PartitionMethod::Key, PartitionOp::Replace);
        key.expression = String::new();
        key.columns = vec!["id".to_owned()];
        key.count = "8".to_owned();
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &key).expect("sql")[0].sql,
            "ALTER TABLE app.t PARTITION BY KEY(id) PARTITIONS 8;"
        );
    }

    /// 新增、删除、重组与移除：RANGE/LIST 走命名分区，HASH/KEY 只能按数量增删。
    #[test]
    fn partition_maintenance_statements_follow_the_method() {
        let mut add = draft(PartitionMethod::Range, PartitionOp::Add);
        add.definitions = vec![def("p2", "300")];
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &add).expect("sql")[0].sql,
            "ALTER TABLE app.t ADD PARTITION (PARTITION p2 VALUES LESS THAN (300));"
        );

        let mut drop = draft(PartitionMethod::List, PartitionOp::Drop);
        drop.targets = vec!["p_east".to_owned(), "p_west".to_owned()];
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &drop).expect("sql")[0].sql,
            "ALTER TABLE app.t DROP PARTITION p_east, p_west;"
        );

        let mut coalesce = draft(PartitionMethod::Hash, PartitionOp::Drop);
        coalesce.count = "2".to_owned();
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &coalesce).expect("sql")[0].sql,
            "ALTER TABLE app.t COALESCE PARTITION 2;"
        );

        let mut reorganize = draft(PartitionMethod::Range, PartitionOp::Reorganize);
        reorganize.targets = vec!["p1".to_owned()];
        reorganize.definitions = vec![def("p1a", "200"), def("p1b", "MAXVALUE")];
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &reorganize).expect("sql")[0].sql,
            "ALTER TABLE app.t REORGANIZE PARTITION p1 INTO (PARTITION p1a VALUES LESS THAN (200), PARTITION p1b VALUES LESS THAN (MAXVALUE));"
        );

        let remove = draft(PartitionMethod::Range, PartitionOp::Remove);
        assert_eq!(
            partition_steps(&snapshot("t"), "t", &remove).expect("sql")[0].sql,
            "ALTER TABLE app.t REMOVE PARTITIONING;"
        );
    }

    /// 缺定义、缺数量、名称无效或方式与操作不匹配时阻断，并说明实际后果的风险。
    #[test]
    fn incomplete_partition_drafts_are_blocked_with_the_real_consequence() {
        let incomplete = draft(PartitionMethod::Range, PartitionOp::Replace);
        let plan = plan_partition(&snapshot("t"), "t", &incomplete, true);
        assert!(!plan.blocked.is_empty());
        assert!(plan.steps.is_empty());

        let bad_count = PartitionDraft { count: "0".to_owned(), ..draft(PartitionMethod::Hash, PartitionOp::Add) };
        assert!(plan_partition(&snapshot("t"), "t", &bad_count, true).blocked.iter().any(|text| text.contains("大于 0")));

        let mut bad_name = draft(PartitionMethod::Range, PartitionOp::Add);
        bad_name.definitions = vec![def("  ", "10")];
        assert!(plan_partition(&snapshot("t"), "t", &bad_name, true).blocked.iter().any(|text| text.contains("分区名")));

        // 删除分区是危险操作：风险文案要说清数据一起消失。
        let mut drop = draft(PartitionMethod::Range, PartitionOp::Drop);
        drop.targets = vec!["p0".to_owned()];
        let plan = plan_partition(&snapshot("t"), "t", &drop, true);
        assert!(plan.blocked.is_empty());
        assert!(plan.risks.iter().any(|risk| risk.level == "danger" && risk.text.contains("删除")));

        // 别的引擎请求时只给拒绝原因，不生成语句。
        let plan = plan_partition(&snapshot("t"), "t", &drop, false);
        assert!(plan.steps.is_empty());
        assert!(plan.blocked.iter().any(|text| text.contains("MySQL")));
    }
}
