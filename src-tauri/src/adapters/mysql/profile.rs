//! MySQL 家族的方言档案：服务器版本、会话 sql_mode 与由此得到的实际能力。
//!
//! 版本与能力判断集中在这里，界面和各个表单不再各自比较版本号。能力按服务器实际报出的版本决定，
//! 不因为“版本号较新”就假定低版本行为已被覆盖；不认识的版本不猜能力，只保留所有版本都成立的部分。
//!
//! 本轮目标版本（R1，基线记录见 docs/verification/2026-11-21-mysql-proprietary-support/baseline.md）：
//! MySQL 5.7 / 8.0 / 8.4 / 9.x，MariaDB 10.6 / 10.11 / 11.4 / 11.8 / 12.x。

use crate::connections::model::Engine;
use crate::ddl::model::LiteralQuoting;

/// 服务器厂商。MariaDB 的版本号与 MySQL 同形但语义不同，不能互相代替。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adapters) enum VersionVendor {
    Mysql,
    Mariadb,
}

/// 由 VERSION() 解析出的版本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adapters) struct ServerVersion {
    pub vendor: VersionVendor,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ServerVersion {
    /// 解析 VERSION() 的原文。MariaDB 在旧客户端面前报 5.5.5- 前缀（复制握手兼容），要跳过它再看真实版本。
    /// 解析不出主次版本时返回 None：能力判断不能按未知版本猜。
    pub(in crate::adapters) fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim();
        if text.is_empty() {
            return None;
        }
        let lower = text.to_ascii_lowercase();
        let vendor = if lower.contains("mariadb") { VersionVendor::Mariadb } else { VersionVendor::Mysql };
        let body = text.strip_prefix("5.5.5-").unwrap_or(text);
        let head: String = body.chars().take_while(|ch| ch.is_ascii_digit() || *ch == '.').collect();
        let mut parts = head.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let minor = parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
        Some(Self { vendor, major, minor, patch })
    }

    /// 版本是否不低于给定主次版本。
    pub(in crate::adapters) fn at_least(&self, major: u32, minor: u32) -> bool {
        (self.major, self.minor) >= (major, minor)
    }

    /// 人类可读的短写，如 8.4.13 或 10.11.6。
    pub(in crate::adapters) fn short(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 服务器实际支持的能力。读不到版本时用 FamilyFeatures::unknown：只保留所有目标版本都成立的项，
/// 并把不确定的项标成不支持，让界面给出可识别的原因，而不是生成服务器不接受的语句。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adapters) struct FamilyFeatures {
    /// 版本读到了。为假时其余项都是保守值。
    pub known: bool,
    pub enum_set: bool,
    pub generated_columns: bool,
    /// 写进语句的存储方式关键字：MySQL 用 STORED，MariaDB 用 PERSISTENT（两者读回都可能是其中一种）。
    pub stored_keyword: &'static str,
    pub partitions: bool,
    pub events: bool,
    pub routines: bool,
    pub triggers: bool,
    /// 会话把反斜杠当转义符（sql_mode 没有 NO_BACKSLASH_ESCAPES）。
    pub backslash_escapes: bool,
}

impl FamilyFeatures {
    /// 读到版本之前 / 读到但解析不出时的保守档案：不声称任何版本相关能力。
    pub(in crate::adapters) const fn unknown() -> Self {
        Self {
            known: false,
            enum_set: false,
            generated_columns: false,
            stored_keyword: "STORED",
            partitions: false,
            events: false,
            routines: false,
            triggers: false,
            backslash_escapes: true,
        }
    }

    /// 按服务器版本与会话 sql_mode 得出能力。这里的版本门槛都是“该能力从哪个版本开始可用”，
    /// 不是“高版本一定和低版本行为相同”：真正的行为差异由真实验收（R9）分组确认。
    pub(in crate::adapters) fn of(version: ServerVersion, sql_mode: &str) -> Self {
        // ENUM/SET 与分区（RANGE/LIST/HASH/KEY）在全部目标版本上都有。
        // 生成列：MySQL 5.7 起；MariaDB 10.2 起（MariaDB 的持久化写法是 PERSISTENT，STORED 是它的同义词）。
        // 事件与例程/触发器：MySQL 5.7 起、MariaDB 10.2 起。
        let generated_columns = match version.vendor {
            VersionVendor::Mysql => version.at_least(5, 7),
            VersionVendor::Mariadb => version.at_least(10, 2),
        };
        Self {
            known: true,
            enum_set: true,
            generated_columns,
            stored_keyword: match version.vendor {
                VersionVendor::Mysql => "STORED",
                VersionVendor::Mariadb => "PERSISTENT",
            },
            partitions: true,
            events: true,
            routines: true,
            triggers: true,
            backslash_escapes: LiteralQuoting::from_sql_mode(sql_mode).backslash_escapes,
        }
    }
}

/// 一条 MySQL 家族连接的方言档案：引擎身份、版本、sql_mode 与能力。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::adapters) struct MysqlProfile {
    pub engine: Engine,
    /// VERSION() 的原文；没读到时为空。
    pub raw_version: String,
    pub version: Option<ServerVersion>,
    pub sql_mode: String,
}

impl MysqlProfile {
    pub(in crate::adapters) fn new(engine: Engine, raw_version: &str, sql_mode: &str) -> Self {
        Self { engine, raw_version: raw_version.to_owned(), version: ServerVersion::parse(raw_version), sql_mode: sql_mode.to_owned() }
    }

    pub(in crate::adapters) fn features(&self) -> FamilyFeatures {
        match self.version {
            Some(version) => FamilyFeatures::of(version, &self.sql_mode),
            None => FamilyFeatures::unknown(),
        }
    }

}

/// 读服务器的 VERSION() 与会话 sql_mode（表结构读取在同一条连接上顺便取，不额外建连）。
pub(super) async fn read_profile_from(conn: &mut mysql_async::Conn, engine: Engine) -> Result<MysqlProfile, String> {
    use mysql_async::prelude::Queryable;
    let row: Option<mysql_async::Row> = conn
        .query_first("SELECT VERSION(), @@session.sql_mode")
        .await
        .map_err(|err| format!("读不到服务器版本：{}", crate::error_text::chain(&err)))?;
    let Some(row) = row else {
        return Err("读不到服务器版本。".to_owned());
    };
    let text = |index: usize| match row.as_ref(index).unwrap_or(&mysql_async::Value::NULL) {
        mysql_async::Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        mysql_async::Value::NULL => String::new(),
        other => format!("{other:?}"),
    };
    Ok(MysqlProfile::new(engine, &text(0), &text(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(raw: &str) -> ServerVersion {
        ServerVersion::parse(raw).unwrap_or_else(|| panic!("解析不出 {raw}"))
    }

    /// 目标版本都要解析成主次修订；MariaDB 的复制握手前缀 5.5.5- 要跳过。
    #[test]
    fn target_versions_parse_including_the_mariadb_handshake_prefix() {
        for raw in ["5.7.44", "8.0.46", "8.4.13", "9.7.4"] {
            let parsed = version(raw);
            assert_eq!(parsed.vendor, VersionVendor::Mysql, "{raw}");
            assert_eq!(parsed.short(), raw);
        }
        for raw in ["10.6.28-MariaDB", "10.11.19-MariaDB-1:10.11.19+maria~ubu2204", "11.4.13-MariaDB", "11.8.9-MariaDB", "12.3.3-MariaDB"] {
            let parsed = version(raw);
            assert_eq!(parsed.vendor, VersionVendor::Mariadb, "{raw}");
            assert!(parsed.at_least(10, 6), "{raw}");
        }
        let handshake = version("5.5.5-10.6.28-MariaDB");
        assert_eq!((handshake.major, handshake.minor, handshake.patch), (10, 6, 28));
        assert_eq!(handshake.vendor, VersionVendor::Mariadb);
    }

    /// 解析不出主次版本时不能猜能力：known 为假，版本相关能力一律保守。
    #[test]
    fn unparsable_versions_do_not_claim_capabilities() {
        assert!(ServerVersion::parse("").is_none());
        assert!(ServerVersion::parse("未知版本").is_none());
        let profile = MysqlProfile::new(Engine::Mysql, "未知版本", "");
        assert!(!profile.features().known);
        assert!(!profile.features().generated_columns);
        assert!(!profile.features().partitions);
        assert!(profile.features().backslash_escapes);
        assert_eq!(profile.features(), FamilyFeatures::unknown());
    }

    /// 目标版本上 ENUM/SET、生成列、分区、事件与例程/触发器都可用；持久化写法的关键字按厂商区分。
    #[test]
    fn target_versions_enable_the_proprietary_objects() {
        for raw in ["5.7.44", "8.0.46", "8.4.13", "9.7.4"] {
            let features = FamilyFeatures::of(version(raw), "");
            assert!(features.known, "{raw}");
            assert!(features.enum_set && features.generated_columns && features.partitions, "{raw}");
            assert!(features.events && features.routines && features.triggers, "{raw}");
            assert_eq!(features.stored_keyword, "STORED", "{raw}");
        }
        for raw in ["10.6.28-MariaDB", "10.11.19-MariaDB", "11.4.13-MariaDB", "11.8.9-MariaDB", "12.3.3-MariaDB"] {
            let features = FamilyFeatures::of(version(raw), "");
            assert!(features.enum_set && features.generated_columns && features.partitions, "{raw}");
            assert_eq!(features.stored_keyword, "PERSISTENT", "{raw}");
        }
    }

    /// sql_mode 决定字面量转义；引擎身份按用户选择保留，版本横幅另外记录厂商。
    #[test]
    fn quoting_follows_the_session_sql_mode() {
        let profile = MysqlProfile::new(Engine::Mysql, "8.4.13", "STRICT_TRANS_TABLES,NO_BACKSLASH_ESCAPES");
        assert!(!profile.features().backslash_escapes);
        assert!(profile.features().known);
        let profile = MysqlProfile::new(Engine::Mysql, "8.4.13", "");
        assert!(profile.features().backslash_escapes);
        // 服务器横幅是 MariaDB 时仍保留用户选择的 MySQL 身份，只把版本与厂商分开记录。
        let profile = MysqlProfile::new(Engine::Mysql, "10.11.19-MariaDB", "");
        assert_eq!(profile.engine, Engine::Mysql);
        assert_eq!(profile.version.map(|version| version.vendor), Some(VersionVendor::Mariadb));
    }
}
