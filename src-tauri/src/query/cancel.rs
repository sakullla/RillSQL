//! 取消分已请求、已确认和失败。经 SSH 时必须另开一条通道。

use std::sync::atomic::{AtomicU64, Ordering};

use mysql_async::prelude::Queryable;

use crate::connections::forward::OpenRecord;
use crate::connections::CancelTransport;

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
            "已请求取消，将另开一条 SSH 通道，等待数据库确认。".to_owned()
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
                view.message = "已通过另一条 SSH 通道发出取消，等待数据库确认。请求发出不等于查询已停止。".to_owned();
            } else if via_ssh && !fresh {
                view.phase = "failed";
                view.message = "经 SSH 的取消没有另开通道，已视为失败。查询仍在执行。".to_owned();
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
pub fn fresh_cancel_channel(opens: &[OpenRecord], busy_id: u64) -> Result<&OpenRecord, String> {
    let busy = opens
        .iter()
        .find(|item| item.id == busy_id)
        .ok_or_else(|| "找不到正在执行的通道".to_owned())?;
    let fresh = opens
        .iter()
        .rev()
        .find(|item| item.id != busy.id)
        .ok_or_else(|| "取消没有另开通道，不能复用正在执行的那条 direct-tcpip".to_owned())?;
    if fresh.host != busy.host || fresh.port != busy.port {
        return Err("取消通道没有指向原来的数据库地址".to_owned());
    }
    if fresh.host == "127.0.0.1" || fresh.host.eq_ignore_ascii_case("localhost") {
        return Err("取消不能把转发器地址当成数据库主机".to_owned());
    }
    Ok(fresh)
}

/// 向数据库发出取消。MySQL / PostgreSQL 都新开传输，不把取消写进正在执行的那条连接。
/// 返回 true 表示这次取消使用了新的传输通道。
pub async fn deliver_cancel(transport: &CancelTransport) -> Result<bool, String> {
    match transport {
        CancelTransport::Sqlite(flag) => {
            flag.store(true, Ordering::SeqCst);
            Ok(false)
        }
        CancelTransport::Mysql { connection_id, opts, via_ssh } => {
            kill_mysql(connection_id, opts, *via_ssh).await?;
            Ok(true)
        }
        CancelTransport::Postgres { token, tls, via_ssh } => {
            cancel_postgres(token, tls.as_ref()).await?;
            if *via_ssh {
                Ok(true)
            } else {
                Ok(true)
            }
        }
        CancelTransport::None => Err("这条已打开的会话没有可取消的数据库通道。查询仍在执行。".to_owned()),
    }
}

async fn kill_mysql(connection_id: &AtomicU64, opts: &mysql_async::Opts, via_ssh: bool) -> Result<(), String> {
    let id = connection_id.load(Ordering::SeqCst);
    if id == 0 {
        return Err("还没有读到正在执行的连接编号，取消失败。查询仍在执行。".to_owned());
    }
    // 新的池会再拨一次传输地址。经 SSH 时该地址是本地转发器，因此是另一次 accept / direct-tcpip。
    let pool = mysql_async::Pool::new(opts.clone());
    let mut conn = pool.get_conn().await.map_err(|err| {
        if via_ssh {
            format!("经 SSH 另开取消通道失败：{err}。查询仍在执行。")
        } else {
            format!("取消失败：{err}。查询仍在执行。")
        }
    })?;
    let statement = format!("KILL QUERY {id}");
    conn.query_drop(statement).await.map_err(|err| format!("取消失败：{err}。查询仍在执行。"))?;
    drop(conn);
    let _ = pool.disconnect().await;
    Ok(())
}

async fn cancel_postgres(token: &tokio_postgres::CancelToken, tls: Option<&tokio_postgres_rustls::MakeRustlsConnect>) -> Result<(), String> {
    // cancel_query 自己另开一条连接，因此不会复用正在执行的流。
    let result = match tls {
        Some(tls) => token.cancel_query(tls.clone()).await,
        None => token.cancel_query(tokio_postgres::NoTls).await,
    };
    result.map_err(|err| format!("取消失败：{err}。查询仍在执行。"))
}

pub fn is_cancel_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("interrupt")
        || lower.contains("canceling statement")
        || lower.contains("cancelling statement")
        || lower.contains("query execution was interrupted")
        || lower.contains("57014")
        || message.contains("已取消")
}
