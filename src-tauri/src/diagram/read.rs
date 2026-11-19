//! 在已经打开的会话上读取关系图。这里不新开数据库连接，也不改会话要连的库。
//!
//! 各引擎的关系图语句与读取在 `adapters/<engine>/diagram.rs`，经目录方言调用。

use crate::adapters::catalog_of;
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::diagram::model::Diagram;

pub(crate) async fn read_diagram(engine: Engine, session: &CatalogSession, database: &str, schema: &str) -> Result<Diagram, String> {
    catalog_of(engine).read_diagram(session, database, schema).await
}

pub(crate) fn failure(err: impl Into<crate::error_text::Reason>) -> String {
    let err = err.into();
    format!("读取关系图失败：{err}")
}

/// 全部引擎的关系图语句（只读校验用）。共用实现的引擎（MySQL 家族）只计一次。
pub(crate) fn diagram_statements() -> Vec<&'static str> {
    let mut statements: Vec<&'static str> = Vec::new();
    for engine in crate::adapters::registry::ENGINES {
        for statement in catalog_of(engine).diagram_statements() {
            if !statements.contains(statement) {
                statements.push(statement);
            }
        }
    }
    statements
}
