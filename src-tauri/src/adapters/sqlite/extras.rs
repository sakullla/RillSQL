//! SQLite 的连接级对象详情（R5）：PRAGMA 与附加库。三个 PRAGMA 各自读取，失败只让对应字段留空。

use crate::catalog::objects::{extras_failure, merge_pragmas, AttachedRow, ExtrasBody};

pub(super) async fn read_sqlite_extras(pool: &sqlx::SqlitePool) -> ExtrasBody {
    let journal_mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
        .fetch_optional(pool)
        .await
        .map_err(|err| extras_failure("journal_mode", err))
        .and_then(|value| value.ok_or_else(|| extras_failure("journal_mode", "没有返回值")));
    let user_version = sqlx::query_scalar::<_, i64>("PRAGMA user_version")
        .fetch_optional(pool)
        .await
        .map_err(|err| extras_failure("user_version", err))
        .and_then(|value| value.ok_or_else(|| extras_failure("user_version", "没有返回值")));
    let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
        .fetch_optional(pool)
        .await
        .map_err(|err| extras_failure("foreign_keys", err))
        .and_then(|value| value.ok_or_else(|| extras_failure("foreign_keys", "没有返回值")));
    let mut body = ExtrasBody {
        pragmas: merge_pragmas(journal_mode, user_version, foreign_keys),
        ..ExtrasBody::default()
    };
    let attached: Result<Vec<(i64, String, String)>, sqlx::Error> = sqlx::query_as("PRAGMA database_list").fetch_all(pool).await;
    match attached {
        Ok(rows) => {
            body.attached.items = rows.into_iter().map(|(_, name, file)| AttachedRow { name, file }).collect();
        }
        Err(err) => body.attached.error = Some(extras_failure("附加库", err)),
    }
    body
}
