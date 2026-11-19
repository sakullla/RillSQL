//! SQLite 的类型规则：类型名只要以字母开头（类型亲和性），其余候选按通用写法。

use crate::adapters::ddl::TypeRules;

pub(in crate::adapters) struct SqliteTypes;

impl TypeRules for SqliteTypes {
    fn valid_type_base(&self, base: &str) -> bool {
        !base.is_empty() && base.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic())
    }
}
