//! 纵向记录视图：把焦点行按"列名—值"两栏列出来，可切到上一行/下一行。
//! 记录视图始终跟着网格的焦点行走：重新读取后焦点按行 id 挂回，那一行不在了就回到第一行。

use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{div, px, AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window};

use crate::i18n::{self, Locale};
use crate::ui::kit::{self, Palette};

use super::session::{CellKind, SharedGrid};
use super::GridPane;

/// 记录视图里的一格：逻辑列、列名、类型和显示文本（含暂存修改）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordField {
    pub column: usize,
    pub name: String,
    pub type_name: String,
    pub label: String,
    pub null: bool,
    /// 这一格显示的只是预览片段；完整值要从查看器读取。
    pub preview: bool,
    /// 这一列在网格里被隐藏了；记录视图仍列出它，只是标出来。
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordView {
    /// 当前显示行号（网格里的位置）。
    pub row: usize,
    /// 行号文本：和网格行号列一致（翻页偏移已算进去，新增行是 +）。
    pub row_number: String,
    pub fields: Vec<RecordField>,
    pub can_prev: bool,
    pub can_next: bool,
}

/// 记录视图显示哪一行：有焦点就是焦点行，否则（或焦点行已不在）是第一行。
pub fn record_row(grid: &SharedGrid) -> Option<usize> {
    let count = grid.loaded_len();
    if count == 0 {
        return None;
    }
    Some(grid.focus_cell().map(|(row, _)| row).filter(|row| *row < count).unwrap_or(0))
}

/// 焦点行的全部逻辑列；没有已加载的行时为空。
pub fn record_view(grid: &SharedGrid) -> Option<RecordView> {
    let row = record_row(grid)?;
    let names = grid.column_names();
    let fields = names
        .into_iter()
        .enumerate()
        .map(|(column, name)| {
            let shown = grid.show_cell(row, column, false);
            RecordField {
                column,
                name,
                type_name: grid.column_type(column).unwrap_or("").to_owned(),
                label: grid.cell_label(row, column),
                null: shown.kind == CellKind::Null,
                preview: shown.preview,
                hidden: grid.column_hidden(column),
            }
        })
        .collect();
    Some(RecordView {
        row,
        row_number: grid.row_number(row),
        fields,
        can_prev: row > 0,
        can_next: row + 1 < grid.loaded_len(),
    })
}

/// 切到上一行/下一行：移动的是网格焦点本身，记录视图和网格高亮始终是同一行。
/// 已经在第一行/最后一行时不动，返回 false。
pub fn record_step(grid: &mut SharedGrid, forward: bool) -> bool {
    let Some(row) = record_row(grid) else {
        return false;
    };
    let target = if forward {
        if row + 1 >= grid.loaded_len() {
            return false;
        }
        row + 1
    } else {
        let Some(target) = row.checked_sub(1) else {
            return false;
        };
        target
    };
    let column = grid.focus_cell().map(|(_, column)| column).or_else(|| grid.visible_columns().first().copied()).unwrap_or(0);
    grid.select(target, column, false);
    true
}

fn pane_command(entity: &Entity<GridPane>, id: &'static str) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let entity = entity.clone();
    move |_, _, cx| {
        entity.update(cx, |pane, cx| {
            let _ = pane.command(id, cx);
        });
    }
}

/// 右侧面板：标题行（行号、上一行、下一行、关闭）和"列名—值"列表。长值点开完整值查看器。
pub fn render_record(view: Option<RecordView>, locale: Locale, p: &Palette, mono: SharedString, entity: &Entity<GridPane>) -> AnyElement {
    let t = |key: &str| SharedString::from(i18n::text(locale, key));
    let (title, can_prev, can_next) = match &view {
        Some(view) => (SharedString::from(i18n::fill(&i18n::text(locale, "grid.record.title"), &[("row", &view.row_number)])), view.can_prev, view.can_next),
        None => (t("grid.record.open"), false, false),
    };
    let header = div()
        .flex_none()
        .h(px(34.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(4.))
        .border_b_1()
        .border_color(p.line)
        .child(div().flex_1().truncate().text_size(px(12.)).text_color(p.text).child(title))
        .child(kit::icon_button("grid-record-prev", IconName::ChevronUp, t("grid.record.prev"), pane_command(entity, "grid-record-prev")).disabled(!can_prev))
        .child(kit::icon_button("grid-record-next", IconName::ChevronDown, t("grid.record.next"), pane_command(entity, "grid-record-next")).disabled(!can_next))
        .child(kit::icon_button("grid-record-close", IconName::X, t("grid.record.close"), pane_command(entity, "grid-record-close")));
    let mut body = div().id("grid-record-fields").flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col();
    match view {
        Some(view) => {
            for field in view.fields {
                let column = field.column;
                let open = {
                    let entity = entity.clone();
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |pane, cx| pane.open_record_value(column, cx));
                    }
                };
                let name = if field.hidden { format!("{} {}", field.name, i18n::text(locale, "grid.record.hidden")) } else { field.name.clone() };
                body = body.child(
                    div()
                        .id(("grid-record-field", column))
                        .debug_selector(move || format!("grid-record-field-{column}"))
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .border_b_1()
                        .border_color(p.line_soft)
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .child(div().truncate().text_size(px(12.)).text_color(p.text_muted).child(SharedString::from(name)))
                                .child(div().truncate().text_size(px(11.)).text_color(p.text_faint).child(SharedString::from(field.type_name.clone()))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .font_family(mono.clone())
                                .text_size(px(12.))
                                .when(field.null, |this| this.italic().text_color(p.text_faint))
                                .when(!field.null, |this| this.text_color(p.text))
                                .child(SharedString::from(field.label.clone())),
                        )
                        .child(kit::icon_button(("grid-record-open", column), IconName::Maximize, t("grid.record.openValue"), open)),
                );
            }
        }
        None => {
            body = body.child(div().p(px(12.)).text_size(px(12.)).text_color(p.text_muted).child(t("grid.record.empty")));
        }
    }
    div()
        .id("grid-record")
        .debug_selector(|| "grid-record".to_owned())
        .role(gpui_kit::Role::Region)
        .aria_label(t("grid.record.open"))
        .flex_none()
        .w(px(300.))
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(p.line)
        .bg(p.chrome)
        .child(header)
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::views::grid::session::{column_of, schema_from_parts, GridColumn, GridRow};

    fn grid(rows: usize) -> SharedGrid {
        let schema = schema_from_parts("sqlite", "", "people", "table", vec![column_of("id", "integer", true), column_of("name", "text", false), column_of("note", "text", false)], vec!["id".to_owned()]).unwrap();
        let columns = crate::app::views::grid::session::columns_from_schema(&schema, true);
        let rows = (0..rows)
            .map(|index| GridRow {
                id: (index + 1).to_string(),
                values: vec![Some((index + 1).to_string()), Some(format!("名{index}")), if index == 0 { None } else { Some("备注".into()) }],
                located: true,
                locator: vec![("id".to_owned(), Some((index + 1).to_string()))],
                inserted: false,
            })
            .collect::<Vec<_>>();
        let mut grid = SharedGrid::new();
        let count = rows.len() as i64;
        grid.load_local(columns, rows, Some(count), Some(schema));
        grid
    }

    #[test]
    fn record_view_lists_every_column_and_steps_until_the_ends() {
        let mut grid = grid(3);
        let view = record_view(&grid).expect("有行");
        assert_eq!(view.row, 0, "没有焦点时显示第一行");
        assert_eq!(view.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), vec!["id", "name", "note"]);
        assert!(view.fields[2].null);
        assert!(!view.can_prev && view.can_next);
        assert!(!record_step(&mut grid, false), "第一行没有上一行");
        assert!(record_step(&mut grid, true));
        let view = record_view(&grid).unwrap();
        assert_eq!(view.row, 1);
        assert_eq!(view.fields[1].label, "名1");
        assert!(record_step(&mut grid, true));
        let view = record_view(&grid).unwrap();
        assert_eq!(view.row, 2);
        assert!(view.can_prev && !view.can_next, "最后一行的下一行禁用");
        assert!(!record_step(&mut grid, true));
        assert_eq!(record_view(&grid).unwrap().row, 2);
        assert!(record_step(&mut grid, false));
        assert_eq!(record_view(&grid).unwrap().row, 1);
    }

    #[test]
    fn record_view_follows_the_focused_row_and_falls_back_after_a_reload() {
        let mut grid = grid(3);
        grid.select(2, 1, false);
        assert_eq!(record_view(&grid).unwrap().fields[0].label, "3");
        // 隐藏列仍在记录视图里，只是被标出来。
        assert!(grid.hide_column(2));
        let view = record_view(&grid).unwrap();
        assert_eq!(view.fields.len(), 3);
        assert!(view.fields[2].hidden);
        // 重新读取：焦点行还在，就按行 id 跟过去（即便位置变了）。
        let columns: Vec<GridColumn> = crate::app::views::grid::session::columns_from_schema(grid.schema().unwrap(), true);
        let reordered = vec![
            GridRow { id: "3".into(), values: vec![Some("3".into()), Some("名2".into()), None], located: true, locator: vec![("id".into(), Some("3".into()))], inserted: false },
            GridRow { id: "1".into(), values: vec![Some("1".into()), Some("名0".into()), None], located: true, locator: vec![("id".into(), Some("1".into()))], inserted: false },
        ];
        let schema = grid.schema().cloned();
        grid.replace_fetched(columns.clone(), reordered, 0, Some(2), false, schema.clone());
        let view = record_view(&grid).unwrap();
        assert_eq!(view.row, 0);
        assert_eq!(view.fields[0].label, "3", "跟着原来的焦点行");
        // 焦点行不在新结果里：回到第一行。
        let without = vec![GridRow { id: "1".into(), values: vec![Some("1".into()), Some("名0".into()), None], located: true, locator: vec![("id".into(), Some("1".into()))], inserted: false }];
        grid.replace_fetched(columns, without, 0, Some(1), false, schema);
        let view = record_view(&grid).unwrap();
        assert_eq!(view.row, 0);
        assert_eq!(view.fields[0].label, "1");
        assert!(!view.can_prev && !view.can_next);
    }
}
