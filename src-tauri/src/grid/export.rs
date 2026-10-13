use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::sql::select_export_gate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Table,
    Query,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportScope {
    Loaded,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportPlan {
    pub allowed: bool,
    pub partial: bool,
    pub file_name: String,
    pub notice: String,
    pub reexecutes: bool,
    pub server_read: bool,
}

pub fn partial_file_name(file_name: &str) -> String {
    if file_name.contains("（部分）") {
        return file_name.to_owned();
    }
    if let Some(stripped) = file_name.strip_suffix(".csv").or_else(|| file_name.strip_suffix(".CSV")) {
        format!("{stripped}（部分）.csv")
    } else {
        format!("{file_name}（部分）")
    }
}

pub fn touch_api() -> usize {
    let loaded = plan_export(Origin::Table, ExportScope::Loaded, None, true, "customers.csv");
    let full = plan_export(Origin::Query, ExportScope::Full, Some("SELECT 1"), false, "query.csv");
    loaded.file_name.len() + full.notice.len() + usize::from(loaded.allowed) + usize::from(full.server_read)
}

pub fn plan_export(origin: Origin, scope: ExportScope, statement: Option<&str>, truncated: bool, file_name: &str) -> ExportPlan {
    let partial = scope == ExportScope::Loaded && truncated;
    let file_name = if partial { partial_file_name(file_name) } else { file_name.to_owned() };
    if scope == ExportScope::Full && origin == Origin::Query {
        return match select_export_gate(statement.unwrap_or("")) {
            Err(reason) => ExportPlan { allowed: false, partial: false, file_name, notice: reason, reexecutes: false, server_read: false },
            Ok(_) => ExportPlan {
                allowed: true,
                partial: false,
                file_name,
                notice: "会重新执行这条 SELECT，并以流式方式写入文件。".to_owned(),
                reexecutes: true,
                server_read: true,
            },
        };
    }
    if partial {
        return ExportPlan {
            allowed: true,
            partial: true,
            file_name,
            notice: "不是完整结果：只包含已经读取的行。".to_owned(),
            reexecutes: false,
            server_read: false,
        };
    }
    if scope == ExportScope::Full {
        return ExportPlan {
            allowed: true,
            partial: false,
            file_name,
            notice: "按当前结构化条件在服务器端读取全部匹配行，不会先把它们填进表格。".to_owned(),
            reexecutes: false,
            server_read: true,
        };
    }
    ExportPlan {
        allowed: true,
        partial: false,
        file_name,
        notice: "当前读取的行已经覆盖这次结果。".to_owned(),
        reexecutes: false,
        server_read: false,
    }
}

pub fn render_csv(columns: &[String], rows: &[Vec<Option<String>>], null_token: &str) -> String {
    let mut lines = Vec::new();
    lines.push(columns.iter().map(|name| quote_csv(name)).collect::<Vec<_>>().join(","));
    for row in rows {
        let fields = columns.iter().enumerate().map(|(index, _)| match row.get(index) {
            Some(Some(value)) => quote_csv(value),
            _ => null_token.to_owned(),
        });
        lines.push(fields.collect::<Vec<_>>().join(","));
    }
    lines.join("\r\n")
}

fn quote_csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteResult {
    pub status: &'static str,
    pub success: bool,
    pub written_rows: usize,
    pub path: Option<PathBuf>,
    pub residual_path: Option<PathBuf>,
    pub message: String,
    pub read_called: bool,
}

pub struct WriteControl {
    pub fail_after_rows: Option<usize>,
    pub fail_message: Option<String>,
    pub cancel_after_rows: Option<usize>,
}

pub fn start_export(
    plan: &ExportPlan,
    acknowledged: bool,
    path: &Path,
    columns: &[String],
    rows: &[Vec<Option<String>>],
    control: Option<&WriteControl>,
    mut read_rows: Option<&mut dyn FnMut() -> Vec<Vec<Option<String>>>>,
) -> WriteResult {
    if !plan.allowed {
        return blocked(plan.notice.clone());
    }
    if plan.reexecutes && !acknowledged {
        return blocked("执行前需说明会重新执行这条 SELECT".to_owned());
    }
    let mut read_called = false;
    let read_owned = if plan.server_read {
        let Some(read) = read_rows.as_mut() else {
            return blocked("没有可重新读取的数据源".to_owned());
        };
        read_called = true;
        Some((*read)())
    } else {
        None
    };
    let source = read_owned.as_deref().unwrap_or(rows);
    let mut result = write_csv(path, columns, source, control);
    result.read_called = read_called;
    if result.success && plan.partial {
        result.message = format!("已导出 {} 行到 {}（仅已读取的行，不是完整结果）", result.written_rows, result.path.as_ref().map(|item| item.display().to_string()).unwrap_or_default());
    }
    result
}

fn blocked(message: String) -> WriteResult {
    WriteResult { status: "blocked", success: false, written_rows: 0, path: None, residual_path: None, message, read_called: false }
}

fn write_csv(path: &Path, columns: &[String], rows: &[Vec<Option<String>>], control: Option<&WriteControl>) -> WriteResult {
    let partial = PathBuf::from(format!("{}.partial", path.display()));
    if let Some(parent) = path.parent() {
        if let Err(error) = fs::create_dir(parent) {
            if error.kind() != io::ErrorKind::AlreadyExists {
                return WriteResult {
                    status: "failed",
                    success: false,
                    written_rows: 0,
                    path: None,
                    residual_path: None,
                    message: format!("路径不可写：{error}。没有生成完整结果。"),
                    read_called: false,
                };
            }
        }
    }
    let header = render_csv(columns, &[], "");
    if let Err(error) = fs::write(&partial, &header) {
        return WriteResult {
            status: "failed",
            success: false,
            written_rows: 0,
            path: None,
            residual_path: None,
            message: format!("路径不可写：{error}。没有生成完整结果。"),
            read_called: false,
        };
    }
    let mut file = match File::options().append(true).open(&partial) {
        Ok(file) => file,
        Err(error) => {
            return WriteResult {
                status: "failed",
                success: false,
                written_rows: 0,
                path: None,
                residual_path: Some(partial.clone()),
                message: format!("{error}。已写入 0 行，不完整的文件保留为 {}，不会冒充完整结果。", partial.display()),
                read_called: false,
            }
        }
    };
    for (index, row) in rows.iter().enumerate() {
        if control.and_then(|item| item.cancel_after_rows) == Some(index) {
            drop(file);
            let _ = fs::remove_file(&partial);
            return WriteResult {
                status: "cancelled",
                success: false,
                written_rows: index,
                path: None,
                residual_path: None,
                message: format!("已取消导出：写入 {index} 行后停止，未完成的文件已删除。"),
                read_called: false,
            };
        }
        if control.and_then(|item| item.fail_after_rows) == Some(index) {
            let message = control.and_then(|item| item.fail_message.clone()).unwrap_or_else(|| "写入失败".to_owned());
            return WriteResult {
                status: "failed",
                success: false,
                written_rows: index,
                path: None,
                residual_path: Some(partial.clone()),
                message: format!("{message}。已写入 {index} 行，不完整的文件保留为 {}，不会冒充完整结果。", partial.display()),
                read_called: false,
            };
        }
        let line = render_csv(columns, std::slice::from_ref(row), "");
        // render_csv includes a header. Data-only line is the second record when header is present.
        let data = line.split("\r\n").nth(1).unwrap_or("");
        if writeln!(file, "\r\n{data}").is_err() {
            return WriteResult {
                status: "failed",
                success: false,
                written_rows: index,
                path: None,
                residual_path: Some(partial),
                message: format!("写入失败。已写入 {index} 行，不完整的文件保留为 {}，不会冒充完整结果。", path.display()),
                read_called: false,
            };
        }
    }
    drop(file);
    if fs::remove_file(path).is_err() && path.exists() {
        return WriteResult {
            status: "failed",
            success: false,
            written_rows: rows.len(),
            path: None,
            residual_path: Some(partial.clone()),
            message: format!("无法替换目标文件。已写入 {} 行，不完整的文件保留为 {}，不会冒充完整结果。", rows.len(), partial.display()),
            read_called: false,
        };
    }
    if let Err(error) = fs::rename(&partial, path) {
        return WriteResult {
            status: "failed",
            success: false,
            written_rows: rows.len(),
            path: None,
            residual_path: Some(partial.clone()),
            message: format!("{error}。已写入 {} 行，不完整的文件保留为 {}，不会冒充完整结果。", rows.len(), partial.display()),
            read_called: false,
        };
    }
    WriteResult {
        status: "done",
        success: true,
        written_rows: rows.len(),
        path: Some(path.to_path_buf()),
        residual_path: None,
        message: format!("已导出 {} 行。", rows.len()),
        read_called: false,
    }
}
