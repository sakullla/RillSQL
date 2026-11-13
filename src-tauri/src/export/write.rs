//! 已读取范围的文件写入：绝对路径、先写 `.partial`，再原子 rename。

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::grid::{insert_line, Cell, Column, Engine};

use super::parse::ParsedTable;
use super::tasks::{Phase, TaskView};
use super::xlsx::write_xlsx;

pub fn partial_name(file_name: &str) -> String {
    const MARK: &str = "（部分）";
    if file_name.contains(MARK) {
        return file_name.to_owned();
    }
    let lower = file_name.to_ascii_lowercase();
    for extension in [".csv", ".json", ".sql", ".md", ".xlsx"] {
        if lower.ends_with(extension) {
            let stem = &file_name[..file_name.len() - extension.len()];
            return format!("{stem}{MARK}{extension}");
        }
    }
    format!("{file_name}{MARK}")
}

#[derive(Clone, Debug)]
pub struct LoadedExport {
    pub id: String,
    pub format: String,
    pub path: PathBuf,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
    pub delimiter: char,
    pub null_token: String,
    pub bom: bool,
    pub table: String,
    pub schema: String,
    pub cancel_after_rows: Option<u64>,
    pub fail_after_rows: Option<u64>,
}

#[derive(Default)]
pub struct ExportControl {
    cancelled: AtomicBool,
    done: AtomicU64,
}

impl ExportControl {
    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); }
    pub fn is_cancelled(&self) -> bool { self.cancelled.load(Ordering::Acquire) }
    pub fn done_rows(&self) -> u64 { self.done.load(Ordering::Relaxed) }
}

impl LoadedExport {
    pub fn write(&self) -> TaskView { self.write_controlled(&ExportControl::default()) }

    pub fn write_controlled(&self, control: &ExportControl) -> TaskView {
        if !self.path.is_absolute() || self.path.file_name().is_none() {
            return TaskView::finish(self.id.clone(), "export", Phase::Blocked, 0, "请选择完整的文件保存路径", None);
        }
        if self.format == "insert" && self.table.trim().is_empty() {
            return TaskView::finish(self.id.clone(), "export", Phase::Blocked, 0, "INSERT 导出需要一张表的名字", None);
        }
        if !matches!(self.format.as_str(), "csv" | "" | "json" | "markdown" | "insert" | "xlsx") {
            return TaskView::finish(self.id.clone(), "export", Phase::Failed, 0, format!("未知导出格式 {}", self.format), None);
        }
        let mut target = self.path.clone();
        if self.truncated {
            if let Some(name) = target.file_name().and_then(|name| name.to_str()) { target.set_file_name(partial_name(name)); }
        }
        let length = self.cancel_after_rows.or(self.fail_after_rows).map(|limit| usize::try_from(limit).unwrap_or(usize::MAX).min(self.rows.len())).unwrap_or(self.rows.len());
        let rows = &self.rows[..length];
        let staging = target.with_file_name(format!(".rillsql-{}.partial", uuid::Uuid::new_v4()));
        let result = if self.format == "xlsx" {
            super::xlsx::write_xlsx_file(&self.columns, rows, &staging, |done| {
                control.done.store(done as u64, Ordering::Relaxed);
                if control.is_cancelled() { Err("已取消导出".to_owned()) } else { Ok(()) }
            }).and_then(|()| File::options().write(true).open(&staging).and_then(|file| file.sync_all()).map_err(|error| error.to_string()))
        } else {
            (|| -> io::Result<()> {
                let file = File::options().write(true).create_new(true).open(&staging)?;
                let mut writer = BufWriter::with_capacity(64 * 1024, file);
                render_text(&mut writer, &self.columns, rows, FormatOptions { format: &self.format, delimiter: self.delimiter, null_token: &self.null_token, bom: self.bom, schema: &self.schema, table: &self.table }, control)?;
                writer.flush()?;
                writer.get_ref().sync_all()
            })().map_err(|error| error.to_string())
        };
        let done = control.done_rows();
        if control.is_cancelled() || self.cancel_after_rows.is_some() {
            let residual = if staging.exists() && fs::remove_file(&staging).is_err() { Some(staging.display().to_string()) } else { None };
            let detail = match &residual {
                Some(path) => format!("已取消，已处理 {done} 行。部分文件未能删除：{path}"),
                None => format!("已取消：实际完成 {done} 行，未完成的文件已删除。"),
            };
            return TaskView::finish(self.id.clone(), "export", Phase::Cancelled, done, detail, residual);
        }
        if let Err(error) = result {
            let residual = staging.exists().then(|| staging.display().to_string());
            return TaskView::finish(self.id.clone(), "export", Phase::Failed, done, format!("写入失败：{error}。已处理 {done} 行，目标文件未被替换。"), residual);
        }
        if self.fail_after_rows.is_some() {
            return TaskView::finish(self.id.clone(), "export", Phase::Failed, done,
                format!("写入失败。已处理 {done} 行，不完整的文件保留为 {}，不会冒充完整结果。", staging.display()), Some(staging.display().to_string()));
        }
        if let Err(error) = fs::rename(&staging, &target) {
            return TaskView::finish(self.id.clone(), "export", Phase::Failed, done,
                format!("无法保存文件：{error}。目标文件未被替换，部分文件保留为 {}。", staging.display()), Some(staging.display().to_string()));
        }
        let phase = if self.truncated { Phase::Partial } else { Phase::Done };
        let note = if self.truncated { "仅已读取的行，不是完整结果。" } else { "已导出全部给出的行。" };
        TaskView::finish(self.id.clone(), "export", phase, done, format!("已导出 {done} 行到 {}。{note}", target.display()), Some(target.display().to_string()))
    }
}

struct FormatOptions<'a> {
    format: &'a str,
    delimiter: char,
    null_token: &'a str,
    bom: bool,
    schema: &'a str,
    table: &'a str,
}

pub fn render(format: &str, table: &ParsedTable, delimiter: char, null_token: &str, bom: bool, schema: &str, table_name: &str) -> Result<Vec<u8>, String> {
    if format == "xlsx" { return write_xlsx(&table.columns, &table.rows); }
    let mut output = Vec::new();
    render_text(&mut output, &table.columns, &table.rows, FormatOptions { format, delimiter, null_token, bom, schema, table: table_name }, &ExportControl::default()).map_err(|error| error.to_string())?;
    Ok(output)
}

fn write_csv_field(writer: &mut impl Write, value: &str) -> io::Result<()> {
    writer.write_all(b"\"")?;
    for (index, part) in value.split('"').enumerate() {
        if index > 0 { writer.write_all(b"\"\"")?; }
        writer.write_all(part.as_bytes())?;
    }
    writer.write_all(b"\"")
}

fn write_markdown_field(writer: &mut impl Write, value: &str) -> io::Result<()> {
    let mut start = 0;
    for (index, ch) in value.char_indices().filter(|(_, ch)| matches!(ch, '|' | '\n' | '\r')) {
        writer.write_all(&value.as_bytes()[start..index])?;
        writer.write_all(if ch == '|' { b"\\|" } else { b" " })?;
        start = index + ch.len_utf8();
    }
    writer.write_all(&value.as_bytes()[start..])
}

fn render_text(writer: &mut impl Write, columns: &[String], rows: &[Vec<Option<String>>], options: FormatOptions<'_>, control: &ExportControl) -> io::Result<()> {
    let mut json_fields = Vec::<(&str, usize)>::new();
    let mut positions = HashMap::<&str, usize>::new();
    let sql_columns: Vec<Column> = if options.format == "insert" { columns.iter().map(|name| Column { name: name.clone(), type_name: "TEXT".into(), nullable: true, primary: false, unique: false, auto: false }).collect() } else { Vec::new() };
    match options.format {
        "csv" | "" => {
            if options.bom { writer.write_all("\u{feff}".as_bytes())?; }
            for (index, name) in columns.iter().enumerate() {
                if index > 0 { write!(writer, "{}", options.delimiter)?; }
                write_csv_field(writer, name)?;
            }
        }
        "json" => {
            for (index, name) in columns.iter().enumerate() {
                if let Some(position) = positions.get(name.as_str()) { json_fields[*position].1 = index; }
                else { positions.insert(name, json_fields.len()); json_fields.push((name, index)); }
            }
            writer.write_all(b"[")?;
        }
        "markdown" => {
            writer.write_all(b"| ")?;
            for (index, name) in columns.iter().enumerate() {
                if index > 0 { writer.write_all(b" | ")?; }
                write_markdown_field(writer, name)?;
            }
            writer.write_all(b" |\r\n| ")?;
            for index in 0..columns.len() {
                if index > 0 { writer.write_all(b" | ")?; }
                writer.write_all(b"---")?;
            }
            writer.write_all(b" |")?;
        }
        "insert" => {},
        other => return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("未知导出格式 {other}"))),
    }
    for (row_index, row) in rows.iter().enumerate() {
        if control.is_cancelled() { return Err(io::Error::new(io::ErrorKind::Interrupted, "已取消导出")); }
        match options.format {
            "csv" | "" => {
                writer.write_all(b"\r\n")?;
                for index in 0..columns.len() {
                    if index > 0 { write!(writer, "{}", options.delimiter)?; }
                    match row.get(index).and_then(Option::as_deref) {
                        Some(text) => write_csv_field(writer, text)?,
                        None => writer.write_all(options.null_token.as_bytes())?,
                    }
                }
            }
            "json" => {
                if row_index > 0 { writer.write_all(b",")?; }
                writer.write_all(b"{")?;
                for (index, (name, column)) in json_fields.iter().enumerate() {
                    if index > 0 { writer.write_all(b",")?; }
                    serde_json::to_writer(&mut *writer, name).map_err(io::Error::other)?;
                    writer.write_all(b":")?;
                    serde_json::to_writer(&mut *writer, &row.get(*column).and_then(Option::as_deref)).map_err(io::Error::other)?;
                }
                writer.write_all(b"}")?;
            }
            "markdown" => {
                writer.write_all(b"\r\n| ")?;
                for index in 0..columns.len() {
                    if index > 0 { writer.write_all(b" | ")?; }
                    match row.get(index).and_then(Option::as_deref) {
                        Some(text) => write_markdown_field(writer, text)?,
                        None => writer.write_all(options.null_token.as_bytes())?,
                    }
                }
                writer.write_all(b" |")?;
            }
            "insert" => {
                if row_index > 0 { writer.write_all(b"\r\n")?; }
                let cells = (0..columns.len()).map(|index| match row.get(index).and_then(Option::as_ref) { Some(text) => Cell::Text(text.clone()), None => Cell::Null }).collect::<Vec<_>>();
                writer.write_all(insert_line(Engine::Sqlite, options.schema, options.table, &sql_columns, columns, &cells).as_bytes())?;
            }
            _ => unreachable!(),
        }
        control.done.store(row_index as u64 + 1, Ordering::Relaxed);
    }
    if options.format == "json" { writer.write_all(b"]")?; }
    Ok(())
}

/// 按目标引擎写 INSERT：标识符用该引擎的引号，列类型决定字面量（数值不加引号，NULL 写 NULL）。
/// 文件导出沿用 SQLite 写法；网格"复制为 INSERT"带上真实引擎和列类型。
pub fn render_insert_for(engine: Engine, schema: &str, table_name: &str, columns: &[Column], table: &ParsedTable) -> String {
    let mut output = String::new();
    for (index, row) in table.rows.iter().enumerate() {
        let cells: Vec<Cell> = table
            .columns
            .iter()
            .enumerate()
            .map(|(index, _)| match row.get(index) {
                Some(Some(text)) => Cell::Text(text.clone()),
                _ => Cell::Null,
            })
            .collect();
        if index > 0 { output.push_str("\r\n"); }
        output.push_str(&insert_line(engine, schema, table_name, columns, &table.columns, &cells));
    }
    output
}
