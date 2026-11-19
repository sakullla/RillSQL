//! Redis 专属说明句的英文：计划风险/阻断/步骤、读取与执行的错误、对象定义提示等只由这个引擎生成的中文句子。
//! 由 `EngineMeta::messages_en` 交给 `ddl::english`，先于共用句型查找；`{}` 等占位规则见 `ddl/english.rs`。

/// (中文句型, 英文句型)。同一前缀里更具体的句型排在前面。
pub(in crate::adapters) const MESSAGES_EN: &[(&str, &str)] = &[
    ("Redis 没有对象定义", "Redis has no object definitions"),
];

/// 重建定义“未包含”清单里这个引擎专属的项。
pub(in crate::adapters) const MISSING_ITEMS_EN: &[(&str, &str)] = &[];
