//! XLSX 编解码。`Workbook::new` 与 `Xlsx::new` 只出现在本文件的读写函数里。

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use calamine::{Data, Reader, Xlsx};
use rust_xlsxwriter::{Formula, Workbook};

use super::parse::ParsedTable;

static WORKBOOK_INITS: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)]
pub fn workbook_initializations() -> u64 {
    WORKBOOK_INITS.load(Ordering::SeqCst)
}

/// 工作簿计数是进程级的。涉及它的测试必须串行，避免并行用例互相改计数。
#[allow(dead_code)]
pub fn workbook_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn touch_workbook() {
    WORKBOOK_INITS.fetch_add(1, Ordering::SeqCst);
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum TypedCell {
    Text(String),
    Number(f64),
    Bool(bool),
    Empty,
    Formula { expr: String, cached: Option<f64> },
}

pub fn write_xlsx(columns: &[String], rows: &[Vec<Option<String>>]) -> Result<Vec<u8>, String> {
    let typed = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| match cell {
                    Some(text) if !text.is_empty() => TypedCell::Text(text.clone()),
                    _ => TypedCell::Empty,
                })
                .collect()
        })
        .collect::<Vec<_>>();
    let header = columns.iter().cloned().map(TypedCell::Text).collect::<Vec<_>>();
    write_book(&[("First", &header, &typed)])
}

#[allow(dead_code)]
pub fn write_typed(header: &[TypedCell], rows: &[Vec<TypedCell>]) -> Result<Vec<u8>, String> {
    write_book(&[("First", header, rows)])
}

#[allow(clippy::type_complexity)]
fn write_book(sheets: &[(&str, &[TypedCell], &[Vec<TypedCell>])]) -> Result<Vec<u8>, String> {
    touch_workbook();
    let mut workbook = Workbook::new();
    for (name, header, rows) in sheets {
        let worksheet = workbook.add_worksheet();
        worksheet.set_name(*name).map_err(|error| error.to_string())?;
        write_typed_row(worksheet, 0, header)?;
        for (index, row) in rows.iter().enumerate() {
            write_typed_row(worksheet, (index + 1) as u32, row)?;
        }
    }
    workbook.save_to_buffer().map_err(|error| error.to_string())
}

fn write_typed_row(worksheet: &mut rust_xlsxwriter::Worksheet, row: u32, cells: &[TypedCell]) -> Result<(), String> {
    for (column, cell) in cells.iter().enumerate() {
        let column = u16::try_from(column).unwrap_or(u16::MAX);
        match cell {
            TypedCell::Empty => {}
            TypedCell::Text(text) => {
                worksheet.write_string(row, column, text).map_err(|error| error.to_string())?;
            }
            TypedCell::Number(value) => {
                worksheet.write_number(row, column, *value).map_err(|error| error.to_string())?;
            }
            TypedCell::Bool(value) => {
                worksheet.write_boolean(row, column, *value).map_err(|error| error.to_string())?;
            }
            TypedCell::Formula { expr, cached } => {
                let mut formula = Formula::new(expr);
                if let Some(value) = cached {
                    formula = formula.set_result(number_text(*value));
                }
                worksheet.write_formula(row, column, formula).map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

pub fn read_xlsx(bytes: &[u8]) -> Result<ParsedTable, String> {
    if bytes.is_empty() {
        return Err("xlsx 文件是空的。".to_owned());
    }
    if !zip_magic(bytes) {
        return Err("第一个工作表不是表。".to_owned());
    }
    if first_sheet_is_chart(bytes) {
        return Err("第一个工作表不是表。".to_owned());
    }
    touch_workbook();
    let mut book: Xlsx<Cursor<&[u8]>> = Xlsx::new(Cursor::new(bytes)).map_err(|_| "xlsx 文件已损坏。".to_owned())?;
    let names = book.sheet_names();
    let Some(name) = names.first().cloned() else {
        return Err("xlsx 文件是空的。".to_owned());
    };
    let range = book.worksheet_range(&name).map_err(|_| "第一个工作表不是表。".to_owned())?;
    if range.start().is_some_and(|(row, _)| row != 0) || range.get_size().1 == 0 {
        return Err("xlsx 缺少列名。".to_owned());
    }
    let mut matrix = range.rows().map(|row| row.iter().map(display_cell).collect::<Vec<_>>()).collect::<Vec<_>>();
    if matrix.is_empty() {
        return Err("xlsx 文件是空的。".to_owned());
    }
    let header = matrix.remove(0);
    if header.is_empty() {
        return Err("xlsx 缺少列名。".to_owned());
    }
    let mut columns = Vec::new();
    for cell in &header {
        let name = cell.as_deref().unwrap_or("").trim();
        if name.is_empty() {
            return Err("xlsx 缺少列名。".to_owned());
        }
        if columns.iter().any(|column: &String| column == name) {
            return Err(format!("xlsx 列名重复：{name}"));
        }
        columns.push(name.to_owned());
    }
    let rows = matrix
        .into_iter()
        .filter(|row| row.iter().any(Option::is_some))
        .map(|row| {
            let mut cells = row;
            cells.resize(columns.len(), None);
            cells.truncate(columns.len());
            cells
        })
        .collect();
    Ok(ParsedTable { columns, rows })
}

fn display_cell(cell: &Data) -> Option<String> {
    match cell {
        Data::Empty => None,
        Data::String(text) if text.is_empty() => None,
        Data::String(text) => Some(text.clone()),
        Data::Float(value) => Some(number_text(*value)),
        Data::Int(value) => Some(value.to_string()),
        Data::Bool(value) => Some(if *value { "true" } else { "false" }.to_owned()),
        Data::DateTimeIso(text) | Data::DurationIso(text) => Some(text.clone()),
        Data::DateTime(_) | Data::Error(_) => None,
    }
}

fn number_text(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        let text = format!("{value}");
        text
    }
}

fn zip_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes[0] == 0x50 && bytes[1] == 0x4b && matches!(bytes[2], 0x03 | 0x05 | 0x07) && matches!(bytes[3], 0x04 | 0x06 | 0x08)
}

fn first_sheet_is_chart(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("spreadsheetml.chartsheet") || text.contains("relationships/chartsheet")
}

#[allow(dead_code)]
pub fn second_sheet_fixture() -> Result<Vec<u8>, String> {
    touch_workbook();
    let mut workbook = Workbook::new();
    let first = workbook.add_worksheet();
    first.set_name("First").map_err(|error| error.to_string())?;
    first.write_string(0, 0, "name").map_err(|error| error.to_string())?;
    first.write_string(1, 0, "甲").map_err(|error| error.to_string())?;
    let second = workbook.add_worksheet();
    second.set_name("Second").map_err(|error| error.to_string())?;
    second.write_string(0, 0, "name").map_err(|error| error.to_string())?;
    second.write_string(1, 0, "只在第二张表").map_err(|error| error.to_string())?;
    workbook.save_to_buffer().map_err(|error| error.to_string())
}

/// 测试用的未压缩 ZIP。图表页放在第一张，后面的工作表不能被当成导入来源。
#[allow(dead_code)]
pub fn chart_first_zip() -> Vec<u8> {
    stored_zip(&[
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/chartsheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Chart1" sheetId="1" r:id="rId1"/><sheet name="Data" sheetId="2" r:id="rId2"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet" Target="chartsheets/sheet1.xml"/></Relationships>"#,
        ),
        ("xl/chartsheets/sheet1.xml", r#"<chartsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"></chartsheet>"#),
        (
            "xl/worksheets/sheet2.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>只在第二张表</t></is></c></row></sheetData></worksheet>"#,
        ),
    ])
}

#[allow(dead_code)]
fn stored_zip(files: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, text) in files {
        let data = text.as_bytes();
        let name_buf = name.as_bytes();
        let offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let crc = crc32(data);
        let mut local = Vec::with_capacity(30 + name_buf.len() + data.len());
        local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        local.extend_from_slice(&20u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&crc.to_le_bytes());
        local.extend_from_slice(&(data.len() as u32).to_le_bytes());
        local.extend_from_slice(&(data.len() as u32).to_le_bytes());
        local.extend_from_slice(&(name_buf.len() as u16).to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(name_buf);
        local.extend_from_slice(data);
        out.extend_from_slice(&local);
        let mut cen = Vec::new();
        cen.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        cen.extend_from_slice(&20u16.to_le_bytes());
        cen.extend_from_slice(&20u16.to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&crc.to_le_bytes());
        cen.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cen.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cen.extend_from_slice(&(name_buf.len() as u16).to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&0u16.to_le_bytes());
        cen.extend_from_slice(&0u32.to_le_bytes());
        cen.extend_from_slice(&offset.to_le_bytes());
        cen.extend_from_slice(name_buf);
        central.extend_from_slice(&cen);
    }
    let central_len = u32::try_from(central.len()).unwrap_or(u32::MAX);
    let central_off = u32::try_from(out.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_len.to_le_bytes());
    out.extend_from_slice(&central_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[allow(dead_code)]
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
