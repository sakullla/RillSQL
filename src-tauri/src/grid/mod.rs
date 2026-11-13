//! 表数据浏览、逐条提交和 CSV 导出。正式路径使用已经打开的连接会话。

pub mod commands;
mod export;
mod live;
mod sql;
mod store;

pub(crate) use live::{
    commit_with_preview as commit_mutations_with_preview, count_rows_search, describe, fetch_cells, foreign_keys, insert_line, query_page_search_preview, query_page_with_schema,
    read_full_cell, request_cancel as request_grid_export_cancel, run_export, sqlserver_rows, ExportMode, ExportSpec, ForeignKey, Mutation,
    MutationKind, LivePage,
};
pub(crate) use sql::{
    delete_sql, insert_sql, looks_like_preview, preview_envelope, qualified_name, quote_ident, reject_literal, reject_literal_with, select_list,
    sql_cell, update_sql, value_family, PreviewValue, ValueFamily, DEFAULT_PREVIEW_BYTES,
};
pub(crate) use store::{refresh_located, Cell, Column, Engine, Filter, FilterOp, Sort, TableSchema};

/// 用现有定位规则判断结果行。调用方只提供列名和单元格，不复制主键比较。
pub(crate) fn locate_sheet<'a>(schema: &TableSchema, columns: &[String], rows: impl IntoIterator<Item = &'a Vec<Option<String>>>) -> Vec<LocatedSheetRow> {
    // 同名列沿用原来的最后一列取值规则；定位只需要唯一键，不能复制正文/BLOB。
    let keys: Vec<_> = schema.key.iter().map(|name| (name, columns.iter().enumerate().filter_map(|(index, column)| (column == name).then_some(index)).collect::<Vec<_>>())).collect();
    let mut stored = rows
        .into_iter()
        .enumerate()
        .map(|(index, values)| {
            let mut cells = std::collections::BTreeMap::new();
            for (name, indices) in &keys {
                let Some(value) = indices.iter().rev().find_map(|index| values.get(*index)) else { continue };
                let cell = match value {
                    Some(text) => Cell::Text(text.clone()),
                    None => Cell::Null,
                };
                cells.insert((*name).clone(), cell);
            }
            store::Row { id: index.to_string(), cells, locator: None, located: false }
        })
        .collect::<Vec<_>>();
    refresh_located(schema, &mut stored);
    stored
        .into_iter()
        .map(|row| LocatedSheetRow {
            located: row.located,
            locator: row
                .locator
                .map(|locator| locator.columns.into_iter().zip(locator.values).map(|(name, cell)| (name, cell_text(cell))).collect())
                .unwrap_or_default(),
        })
        .collect()
}

fn cell_text(cell: Cell) -> Option<String> {
    match cell {
        Cell::Null => None,
        Cell::Text(text) => Some(text),
    }
}

pub(crate) struct LocatedSheetRow {
    pub located: bool,
    pub locator: Vec<(String, Option<String>)>,
}

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name::<export::ExportPlan>(),
        std::any::type_name::<store::TableStore>(),
        std::any::type_name_of_val(&sql::select_export_gate),
        std::any::type_name_of_val(&store::query_page),
        std::any::type_name_of_val(&store::commit_changes),
        std::any::type_name_of_val(&export::plan_export),
        std::any::type_name_of_val(&export::touch_api),
        std::any::type_name_of_val(&export::start_export),
        std::any::type_name_of_val(&commands::grid_page),
        std::any::type_name_of_val(&live::describe),
        std::any::type_name_of_val(&store::customers_fixture),
        std::any::type_name_of_val(&store::nokey_fixture),
        std::any::type_name_of_val(&store::touch_api),
    );
    let _ = store::touch_api();
    let _ = export::touch_api();
    "grid"
}

#[cfg(test)]
mod filter_tests;
#[cfg(test)]
mod tests;
