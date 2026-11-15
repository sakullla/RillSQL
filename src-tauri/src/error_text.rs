//! 驱动错误的展示文本。
//!
//! 有些驱动的 `Display` 只写一句类别，真正的原因放在 `source()` 里：
//! tokio-postgres 的服务器报错只显示 “db error”，连接失败只显示 “error connecting to server”。
//! 直接 `to_string()` 会把语法错误、关系不存在、拒绝连接这些关键信息丢掉。
//! 这里沿 `source()` 链补齐原因；已经包含在前文里的原因不重复拼接。

use std::error::Error;

/// 错误及其原因链的完整文本。PostgreSQL 服务器报错直接取服务器给的级别、消息、DETAIL 与 HINT。
pub fn chain<E: Error + 'static>(err: &E) -> String {
    chain_dyn(err)
}

/// `chain` 的动态版本，供已经是 `dyn Error` 的调用方使用。
pub fn chain_dyn(err: &(dyn Error + 'static)) -> String {
    if let Some(db) = postgres_db_error(err) {
        return postgres_text(db);
    }
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        if let Some(db) = cause.downcast_ref::<tokio_postgres::error::DbError>() {
            return postgres_text(db);
        }
        let part = cause.to_string();
        let part = part.trim();
        if !part.is_empty() && !text.contains(part) {
            text = format!("{text}: {part}");
        }
        source = cause.source();
    }
    collapse_repeats(&text)
}

/// 有的驱动把同一句类别在外层和内层各写一遍（"Input/output error: Input/output error: ..."）。
/// 相邻重复的段只留一段，原因本身不动。
fn collapse_repeats(text: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in text.split(": ") {
        if parts.last().is_some_and(|last| last.trim() == part.trim()) {
            continue;
        }
        parts.push(part);
    }
    parts.join(": ")
}

/// 失败原因的统一入参：文案或驱动错误都能直接传进来。驱动错误按 `chain` 取完整原因，
/// 而不是只取 `Display` 那一句类别。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason(pub String);

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Reason {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for Reason {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&String> for Reason {
    fn from(value: &String) -> Self {
        Self(value.clone())
    }
}

macro_rules! reason_from_errors {
    ($($ty:ty),* $(,)?) => {$(
        impl From<$ty> for Reason {
            fn from(value: $ty) -> Self {
                Self(chain(&value))
            }
        }
        impl From<&$ty> for Reason {
            fn from(value: &$ty) -> Self {
                Self(chain(value))
            }
        }
    )*};
}

reason_from_errors!(
    tokio_postgres::Error,
    mysql_async::Error,
    sqlx::Error,
    tiberius::error::Error,
    std::io::Error,
    mongodb::error::Error,
    std::string::FromUtf8Error,
);

/// 服务器报错：消息本身，再按需带上 DETAIL / HINT（与 psql 的写法一致，用户一眼认得）。
/// 级别（ERROR / FATAL）在错误面板里是冗余信息，不重复写。
fn postgres_text(db: &tokio_postgres::error::DbError) -> String {
    let mut text = db.message().trim().to_owned();
    if let Some(detail) = db.detail().map(str::trim).filter(|value| !value.is_empty()) {
        text.push_str("
DETAIL: ");
        text.push_str(detail);
    }
    if let Some(hint) = db.hint().map(str::trim).filter(|value| !value.is_empty()) {
        text.push_str("
HINT: ");
        text.push_str(hint);
    }
    text
}

fn postgres_db_error<'a>(err: &'a (dyn Error + 'static)) -> Option<&'a tokio_postgres::error::DbError> {
    err.downcast_ref::<tokio_postgres::Error>()?.as_db_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[derive(Debug)]
    struct Outer(Inner);
    #[derive(Debug)]
    struct Inner(&'static str);

    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("error connecting to server")
        }
    }
    impl Error for Outer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }
    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }
    impl Error for Inner {}

    #[test]
    fn appends_hidden_causes() {
        let err = Outer(Inner("Connection refused (os error 10061)"));
        assert_eq!(chain(&err), "error connecting to server: Connection refused (os error 10061)");
    }

    #[test]
    fn skips_causes_already_in_the_text() {
        let err = Outer(Inner("error connecting"));
        assert_eq!(chain(&err), "error connecting to server");
    }

    #[test]
    fn repeated_categories_are_written_once() {
        let err = Inner("Input/output error: Input/output error: invalid peer certificate: UnknownIssuer");
        assert_eq!(chain(&err), "Input/output error: invalid peer certificate: UnknownIssuer");
    }

    #[test]
    fn plain_errors_stay_unchanged() {
        let err = Inner("relation does not exist");
        assert_eq!(chain(&err), "relation does not exist");
    }
}
