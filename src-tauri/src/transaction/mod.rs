//! 连接级事务核心模型：共享快照、统一效果与纯方言控制模型。（ADR-2、ADR-3）
//!
//! 查询、网格和结构业务共用同一个 [`TransactionSnapshot`]，所有执行结果用 [`TransactionEffect`]
//! 报告事务效果；方言只通过 [`TransactionDialect`] 提供能力边界、控制语句 SQL 与手写语句分类。
//! 本模块不依赖驱动与 GPUI，状态只在数据库确认后推进。

pub mod dialect;
pub mod model;

pub use dialect::{
    classify_control, AbortedRecovery, ControlMatch, ControlSql, ControlSyntax, DuplicateBegin, ImplicitCommitSupport, ImplicitCommitVerdict, SettingChange,
    SettingOption, SettingSupport, TransactionCapabilities, TransactionDialect, READ_COMMITTED, READ_ONLY, READ_UNCOMMITTED, READ_WRITE, REPEATABLE_READ,
    SERIALIZABLE, SNAPSHOT,
};
pub use model::{
    ControlOutcome, ControlStatement, SavepointName, StatementFailure, StatementOutcome, TransactionEffect, TransactionError, TransactionMachine, TransactionProbe,
    TransactionSettings, TransactionSnapshot, TransactionState, UnknownReason, UnknownResolution, UnknownTransaction,
};

pub fn module_id() -> &'static str {
    let machine = TransactionMachine::new(String::new(), TransactionCapabilities::UNSUPPORTED);
    let _linked = (
        machine.snapshot(),
        std::any::type_name::<TransactionMachine>(),
        std::any::type_name::<TransactionSnapshot>(),
        std::any::type_name::<TransactionCapabilities>(),
        std::any::type_name::<UnknownTransaction>(),
        std::any::type_name_of_val(&classify_control),
        std::any::type_name_of_val(&TransactionMachine::precheck),
        std::any::type_name_of_val(&TransactionMachine::acknowledge),
        std::any::type_name_of_val(&TransactionMachine::observe_statement),
        std::any::type_name_of_val(&TransactionMachine::observe_implicit_commit),
        std::any::type_name_of_val(&TransactionMachine::observe_probe),
        std::any::type_name_of_val(&TransactionMachine::resolve_unknown),
        std::any::type_name_of_val(&TransactionMachine::reconnect),
    );
    let _ = _linked;
    "transaction"
}

#[cfg(test)]
mod tests;
