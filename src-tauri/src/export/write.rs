//! 已读取范围的文件写入：绝对路径、先写 `.partial`，再原子 rename。

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

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

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("请选择完整的文件保存路径".to_owned());
    }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).ok_or_else(|| "请选择完整的文件保存路径".to_owned())?;
    if !parent.exists() {
        return Err(format!("路径不可写：目录不存在 {}。没有生成完整结果。", parent.display()));
    }
    let staging = path.with_file_name(format!(".rillsql-{}.partial", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = File::create(&staging)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&staging, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&staging);
        return Err(format!("无法保存文件：{error}。目标文件未被替换。"));
    }
    Ok(())
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

impl LoadedExport {
    pub fn write(&self) -> TaskView {
        if !self.path.is_absolute() {
            return TaskView::finish(self.id.clone(), "export", Phase::Blocked, 0, "请选择完整的文件保存路径", None);
        }
        if self.format == "insert" && self.table.trim().is_empty() {
            return TaskView::finish(self.id.clone(), "export", Phase::Blocked, 0, "INSERT 导出需要一张表的名字", None);
        }
        let mut target = self.path.clone();
        if self.truncated {
            if let Some(name) = target.file_name().and_then(|name| name.to_str()) {
                target.set_file_name(partial_name(name));
            }
        }
        let limit = self.cancel_after_rows.or(self.fail_after_rows);
        let rows = match limit {
            Some(limit) => self.rows.iter().take(limit as usize).cloned().collect(),
            None => self.rows.clone(),
        };
        let table = ParsedTable { columns: self.columns.clone(), rows };
        let bytes = match render(&self.format, &table, self.delimiter, &self.null_token, self.bom, &self.schema, &self.table) {
            Ok(bytes) => bytes,
            Err(message) => return TaskView::finish(self.id.clone(), "export", Phase::Failed, 0, message, None),
        };
        if let Some(stop) = self.cancel_after_rows {
            let staging = target.with_file_name(format!(".rillsql-{}.partial", uuid::Uuid::new_v4()));
            if File::create(&staging).and_then(|mut file| file.write_all(&bytes)).is_ok() {
                let _ = fs::remove_file(&staging);
            }
            return TaskView::finish(
                self.id.clone(),
                "export",
                Phase::Cancelled,
                stop.min(self.rows.len() as u64),
                format!("已取消：实际写入 {} 行后停止，未完成的文件已删除。", stop.min(self.rows.len() as u64)),
                None,
            );
        }
        if let Some(stop) = self.fail_after_rows {
            let staging = target.with_file_name(format!(".rillsql-{}.partial", uuid::Uuid::new_v4()));
            let _ = File::create(&staging).and_then(|mut file| file.write_all(&bytes));
            return TaskView::finish(
                self.id.clone(),
                "export",
                Phase::Failed,
                stop.min(self.rows.len() as u64),
                format!("写入失败。已写入 {} 行，不完整的文件保留为 {}，不会冒充完整结果。", stop, staging.display()),
                Some(staging.display().to_string()),
            );
        }
        match write_atomic(&target, &bytes) {
            Ok(()) => {
                let phase = if self.truncated { Phase::Partial } else { Phase::Done };
                let note = if self.truncated { "仅已读取的行，不是完整结果。" } else { "已导出全部给出的行。" };
                TaskView::finish(
                    self.id.clone(),
                    "export",
                    phase,
                    table.rows.len() as u64,
                    format!("已导出 {} 行到 {}。{note}", table.rows.len(), target.display()),
                    Some(target.display().to_string()),
                )
            }
            Err(message) => TaskView::finish(self.id.clone(), "export", Phase::Failed, 0, message, None),
        }
    }
}

pub fn render(format: &str, table: &ParsedTable, delimiter: char, null_token: &str, bom: bool, schema: &str, table_name: &str) -> Result<Vec<u8>, String> {
    let text = match format {
        "csv" | "" => render_csv(table, delimiter, null_token, bom),
        "json" => render_json(table),
        "markdown" => render_markdown(table, null_token),
        "insert" => render_insert(table, schema, table_name),
        "xlsx" => return write_xlsx(&table.columns, &table.rows),
        other => return Err(format!("未知导出格式 {other}")),
    };
    Ok(text.into_bytes())
}

fn render_csv(table: &ParsedTable, delimiter: char, null_token: &str, bom: bool) -> String {
    let mut lines = Vec::new();
    lines.push(table.columns.iter().map(|name| quote_csv(name)).collect::<Vec<_>>().join(&delimiter.to_string()));
    for row in &table.rows {
        let fields = table.columns.iter().enumerate().map(|(index, _)| match row.get(index) {
            Some(Some(value)) => quote_csv(value),
            _ => null_token.to_owned(),
        });
        lines.push(fields.collect::<Vec<_>>().join(&delimiter.to_string()));
    }
    let body = lines.join("\r\n");
    if bom { format!("\u{feff}{body}") } else { body }
}

fn quote_csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn render_json(table: &ParsedTable) -> String {
    let rows = table
        .rows
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::new();
            for (index, name) in table.columns.iter().enumerate() {
                let value = match row.get(index) {
                    Some(Some(text)) => serde_json::Value::String(text.clone()),
                    _ => serde_json::Value::Null,
                };
                object.insert(name.clone(), value);
            }
            serde_json::Value::Object(object)
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_owned())
}

fn render_markdown(table: &ParsedTable, null_token: &str) -> String {
    if !null_token.is_empty() {
        return render_markdown_token(table, null_token);
    }
    let mut lines = vec![markdown_row(table.columns.iter().map(|name| Some(name.as_str()))), format!("| {} |", vec!["---"; table.columns.len()].join(" | "))];
    for row in &table.rows {
        lines.push(markdown_row(table.columns.iter().enumerate().map(|(index, _)| match row.get(index) {
            Some(Some(text)) => Some(text.as_str()),
            _ => None,
        })));
    }
    lines.join("\r\n")
}

fn render_markdown_token(table: &ParsedTable, null_token: &str) -> String {
    let header = markdown_row(table.columns.iter().map(|name| Some(name.as_str())));
    let rule = format!("| {} |", vec!["---"; table.columns.len()].join(" | "));
    let mut lines = vec![header, rule];
    for row in &table.rows {
        let cells = table.columns.iter().enumerate().map(|(index, _)| match row.get(index) {
            Some(Some(text)) => text.replace('|', "\\|").replace(['\n', '\r'], " "),
            _ => null_token.to_owned(),
        });
        lines.push(format!("| {} |", cells.collect::<Vec<_>>().join(" | ")));
    }
    lines.join("\r\n")
}

fn markdown_row<'a>(cells: impl Iterator<Item = Option<&'a str>>) -> String {
    let fields = cells
        .map(|cell| match cell {
            Some(text) => text.replace('|', "\\|").replace(['\n', '\r'], " "),
            None => String::new(),
        })
        .collect::<Vec<_>>();
    format!("| {} |", fields.join(" | "))
}

fn render_insert(table: &ParsedTable, schema: &str, table_name: &str) -> String {
    let columns: Vec<Column> = table
        .columns
        .iter()
        .map(|name| Column { name: name.clone(), type_name: "TEXT".to_owned(), nullable: true, primary: false, unique: false, auto: false })
        .collect();
    let mut lines = Vec::new();
    for row in &table.rows {
        let cells: Vec<Cell> = table
            .columns
            .iter()
            .enumerate()
            .map(|(index, _)| match row.get(index) {
                Some(Some(text)) => Cell::Text(text.clone()),
                _ => Cell::Null,
            })
            .collect();
        lines.push(insert_line(Engine::Sqlite, schema, table_name, &columns, &table.columns, &cells));
    }
    lines.join("\r\n")
}
