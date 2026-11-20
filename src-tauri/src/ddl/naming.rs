//! 索引命名模板（R8）：普通索引、唯一索引各一个模板，加列名之间的连接符。
//! 模板占位符：`{table}` 表名，`{columns}` 全部列名（按连接符连接），`{column1}` 第一列。
//! 生成的名字按方言的长度上限截断，截断时末尾加 `_` 和 6 位哈希，保证不同的长名字截断后仍然不同。

use serde::{Deserialize, Serialize};

pub const DEFAULT_NORMAL: &str = "idx_{table}_{columns}";
pub const DEFAULT_UNIQUE: &str = "uk_{table}_{columns}";
pub const DEFAULT_JOINER: &str = "_";
/// 方言没有声明上限时使用的保守长度（与旧版行为一致）。
pub const FALLBACK_LIMIT: usize = 60;
/// 截断后缀：`_` 加 6 位十六进制哈希。
const HASH_SUFFIX_LEN: usize = 7;

/// 存进 app-state 的命名模板。旧文件没有这一项，或缺某个字段时取默认值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IndexNaming {
    pub normal: String,
    pub unique: String,
    pub joiner: String,
}

impl Default for IndexNaming {
    fn default() -> Self {
        Self { normal: DEFAULT_NORMAL.to_owned(), unique: DEFAULT_UNIQUE.to_owned(), joiner: DEFAULT_JOINER.to_owned() }
    }
}

impl IndexNaming {
    /// 模板留空时退回默认模板，连接符留空时允许（列名直接相连）。
    fn template(&self, unique: bool) -> &str {
        let (value, fallback) = if unique { (&self.unique, DEFAULT_UNIQUE) } else { (&self.normal, DEFAULT_NORMAL) };
        if value.trim().is_empty() { fallback } else { value.trim() }
    }

    /// 按模板生成，不截断。没有列（或列名都为空）时返回空串。
    pub fn render(&self, table: &str, unique: bool, columns: &[&str]) -> String {
        let columns: Vec<&str> = columns.iter().map(|name| name.trim()).filter(|name| !name.is_empty()).collect();
        let Some(first) = columns.first() else {
            return String::new();
        };
        let table = if table.trim().is_empty() { "table" } else { table.trim() };
        self.template(unique)
            .replace("{table}", table)
            .replace("{columns}", &columns.join(&self.joiner))
            .replace("{column1}", first)
    }

    /// 按模板生成并按上限截断。`limit` 为 None 表示不限长度。
    pub fn name(&self, table: &str, unique: bool, columns: &[&str], limit: Option<usize>) -> String {
        fit_identifier(&self.render(table, unique, columns), limit)
    }
}

/// 方言的索引名长度上限：MySQL/MariaDB 64，PostgreSQL 63，SQL Server 128，SQLite 不限；没有 DDL 方言的引擎按 FALLBACK_LIMIT。
pub fn index_name_limit(engine: &str) -> Option<usize> {
    crate::adapters::ddl::ddl_named(engine).map_or(Some(FALLBACK_LIMIT), |dialect| dialect.max_index_name_len())
}

/// 不超过上限时原样返回；超过时截到 `limit - 7` 字节（落在字符边界上），再接 `_` 和 6 位哈希。
pub fn fit_identifier(name: &str, limit: Option<usize>) -> String {
    let Some(limit) = limit else {
        return name.to_owned();
    };
    if name.len() <= limit {
        return name.to_owned();
    }
    if limit <= HASH_SUFFIX_LEN {
        return truncate_bytes(name, limit).to_owned();
    }
    let head = truncate_bytes(name, limit - HASH_SUFFIX_LEN);
    format!("{head}_{}", short_hash(name))
}

fn truncate_bytes(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// FNV-1a 32 位取低 24 位，写成 6 位十六进制。跨版本、跨平台稳定。
fn short_hash(text: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{:06x}", hash & 0x00ff_ffff)
}

/// 与当前表已有索引重名（大小写不敏感）。
pub fn collides(existing: &[String], name: &str) -> bool {
    let wanted = name.trim();
    !wanted.is_empty() && existing.iter().any(|item| item.eq_ignore_ascii_case(wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_templates_reproduce_the_old_names() {
        let naming = IndexNaming::default();
        assert_eq!(naming.render("orders", false, &["user_id", "created_at"]), "idx_orders_user_id_created_at");
        assert_eq!(naming.render("orders", true, &["email"]), "uk_orders_email");
        assert_eq!(naming.render("orders", false, &[]), "", "没有列时不生成名字");
        assert_eq!(naming.render("  ", false, &["a"]), "idx_table_a", "表名为空时用占位");
    }

    #[test]
    fn custom_templates_use_every_placeholder_and_the_joiner() {
        let naming = IndexNaming { normal: "ix__{table}__{columns}".into(), unique: "{table}_{column1}_uq".into(), joiner: "$".into() };
        assert_eq!(naming.render("t", false, &["a", "b", "c"]), "ix__t__a$b$c");
        assert_eq!(naming.render("t", true, &["a", "b"]), "t_a_uq");
        let blank = IndexNaming { normal: "  ".into(), unique: String::new(), joiner: String::new() };
        assert_eq!(blank.render("t", false, &["a", "b"]), "idx_t_ab", "空模板退回默认，空连接符直接相连");
        assert_eq!(blank.render("t", true, &["a"]), "uk_t_a");
    }

    #[test]
    fn old_state_without_a_field_reads_defaults() {
        let partial: IndexNaming = serde_json::from_str(r#"{"normal":"i_{columns}"}"#).unwrap();
        assert_eq!(partial.normal, "i_{columns}");
        assert_eq!(partial.unique, DEFAULT_UNIQUE);
        assert_eq!(partial.joiner, DEFAULT_JOINER);
    }

    #[test]
    fn each_dialect_truncates_to_its_limit_with_a_hash_suffix() {
        assert_eq!(index_name_limit("mysql"), Some(64));
        assert_eq!(index_name_limit("mariadb"), Some(64));
        assert_eq!(index_name_limit("postgres"), Some(63));
        assert_eq!(index_name_limit("sqlserver"), Some(128));
        assert_eq!(index_name_limit("sqlite"), None);
        assert_eq!(index_name_limit("unknown-engine"), Some(FALLBACK_LIMIT));

        let naming = IndexNaming::default();
        let long_cols: Vec<String> = (0..12).map(|ix| format!("column_number_{ix}")).collect();
        let cols: Vec<&str> = long_cols.iter().map(String::as_str).collect();
        let full = naming.render("very_long_table_name", false, &cols);
        assert!(full.len() > 128);
        for (engine, limit) in [("mysql", 64), ("mariadb", 64), ("postgres", 63), ("sqlserver", 128)] {
            let name = naming.name("very_long_table_name", false, &cols, index_name_limit(engine));
            assert_eq!(name.len(), limit, "{engine} 截到上限");
            let (head, hash) = name.split_at(limit - 7);
            assert!(full.starts_with(head), "{engine} 保留前缀");
            assert!(hash.starts_with('_') && hash[1..].chars().all(|ch| ch.is_ascii_hexdigit()) && hash.len() == 7, "{engine}: {hash}");
        }
        assert_eq!(naming.name("very_long_table_name", false, &cols, index_name_limit("sqlite")), full, "SQLite 不截断");

        // 前缀相同、只有末尾不同的两个长名字，截断后仍然不同。
        let mut other = cols.clone();
        other.push("tail_a");
        let mut another = cols.clone();
        another.push("tail_b");
        let a = naming.name("very_long_table_name", false, &other, Some(64));
        let b = naming.name("very_long_table_name", false, &another, Some(64));
        assert_ne!(a, b);
        assert_eq!(a, naming.name("very_long_table_name", false, &other, Some(64)), "同一输入结果稳定");
        assert_eq!(naming.name("t", false, &["a"], Some(64)), "idx_t_a", "短名字不动");
    }

    #[test]
    fn truncation_stays_on_character_boundaries() {
        let name = "索".repeat(40);
        let fitted = fit_identifier(&name, Some(63));
        assert!(fitted.len() <= 63);
        assert!(fitted.ends_with(&format!("_{}", short_hash(&name))));
    }

    #[test]
    fn collision_check_ignores_case() {
        let existing = vec!["idx_orders_user".to_owned(), "PRIMARY".to_owned()];
        assert!(collides(&existing, "IDX_orders_user"));
        assert!(collides(&existing, " primary "));
        assert!(!collides(&existing, "idx_orders_other"));
        assert!(!collides(&existing, ""));
    }
}
