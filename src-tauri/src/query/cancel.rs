//! 取消分已请求、已确认和失败。经 SSH 时必须另开一条通道。

#[cfg(test)]
use crate::connections::forward::OpenRecord;
use crate::connections::CancelTransport;

/// 取消语句的 SQL 由 SQL Server 适配器生成。
#[cfg(test)]
pub use crate::adapters::sqlserver::connect::kill_sql;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelView {
    pub phase: &'static str,
    pub message: String,
    pub fresh_channel: bool,
}

pub fn on_request(via_ssh: bool) -> CancelView {
    CancelView {
        phase: "requested",
        fresh_channel: false,
        message: if via_ssh {
            "已请求取消，将另开一条经 SSH 的连接通知数据库，等待确认。".to_owned()
        } else {
            "已请求取消，等待数据库确认。".to_owned()
        },
    }
}

pub fn on_delivery(view: &mut CancelView, result: Result<bool, String>, via_ssh: bool) {
    match result {
        Ok(fresh) => {
            view.fresh_channel = fresh;
            if via_ssh && fresh {
                view.message = "已通过另一条经 SSH 的连接发出取消，等待数据库确认。请求发出不等于查询已停止。".to_owned();
            } else if via_ssh && !fresh {
                view.phase = "failed";
                view.message = "经 SSH 的取消没能另开连接，已视为失败。查询仍在执行。".to_owned();
                view.fresh_channel = false;
            }
        }
        Err(message) => {
            view.phase = "failed";
            view.fresh_channel = false;
            view.message = message;
        }
    }
}

pub fn on_statement_end(view: &mut CancelView, outcome: &str) {
    if view.phase != "requested" {
        return;
    }
    if outcome == "cancelled" {
        view.phase = "confirmed";
        view.message = "数据库已确认取消。".to_owned();
    }
}

/// 执行通道已经占用 `busy_id`。取消必须是另一次打开，不能复用那条记录。
#[cfg(test)]
pub fn fresh_cancel_channel(opens: &[OpenRecord], busy_id: u64) -> Result<&OpenRecord, String> {
    let busy = opens
        .iter()
        .find(|item| item.id == busy_id)
        .ok_or_else(|| "找不到正在执行的通道".to_owned())?;
    let fresh = opens
        .iter()
        .rev()
        .find(|item| item.id != busy.id)
        .ok_or_else(|| "取消没有另开连接，不能复用正在执行的那一条".to_owned())?;
    if fresh.host != busy.host || fresh.port != busy.port {
        return Err("取消用的连接没有指向原来的数据库地址".to_owned());
    }
    if fresh.host == "127.0.0.1" || fresh.host.eq_ignore_ascii_case("localhost") {
        return Err("取消不能把转发器地址当成数据库主机".to_owned());
    }
    Ok(fresh)
}

/// 向数据库发出取消。取消通道由各引擎适配器提供：远程引擎都新开传输，不把取消写进正在执行的那条连接。
/// 返回 true 表示这次取消使用了新的传输通道。
pub async fn deliver_cancel(transport: &CancelTransport) -> Result<bool, String> {
    transport.deliver().await
}

pub fn is_cancel_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("interrupt")
        || lower.contains("canceling statement")
        || lower.contains("cancelling statement")
        || lower.contains("query execution was interrupted")
        || lower.contains("57014")
        // KILL spid 后正在执行的会话收到这两类报错。
        || lower.contains("killed")
        || lower.contains("severe error occurred")
        || message.contains("已取消")
}
