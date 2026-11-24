//! 日期时间选择器（R5）：替代 gpui-kit 的 Calendar，按界面语言给出周一起始的星期与月份，
//! 年月网格快速切换，时 / 分 / 秒 / 毫秒逐段输入，以及“现在 / 今天 / 设为 NULL / 恢复原值”。
//!
//! 这里只负责画：状态（当前显示哪一月、哪一种网格）与输入框由调用方持有，
//! 所有操作经 `on` 回调交回调用方，日期与时间的合成、拆分是 `grid::temporal` 里的纯函数。

use std::rc::Rc;

use chrono::{Datelike, NaiveDate};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{div, px, AnyElement, App, Entity, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, TestSupportExt as _, Window};

use crate::app::views::grid::temporal::{self, TemporalKind, TimeField};
use crate::i18n::{self, Locale};
use crate::ui::kit::{self, ControlSize, Palette, Radius, Space, TypeScale};

/// 选择器当前显示的网格。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickerView {
    #[default]
    Days,
    Months,
    Years,
}

/// 选择器的浏览位置：正在看的网格与年月。`None` 的年月表示跟随输入框里的日期。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PickerCursor {
    pub view: PickerView,
    pub month: Option<(i32, u32)>,
}

/// 选择器发出的操作；调用方据此改写输入框或浏览位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerAction {
    /// 上一页：日网格翻一个月，月份网格翻一年，年份网格翻十年。
    Prev,
    Next,
    ShowMonths,
    ShowYears,
    PickDay(NaiveDate),
    PickMonth(u32),
    PickYear(i32),
    /// ↑/↓ 调整某一段时间。
    Step(TimeField, i64),
    Now,
    Today,
    Null,
    Restore,
}

pub type PickerHandler = Rc<dyn Fn(PickerAction, &mut Window, &mut App)>;

/// 画一次选择器需要的全部输入。
pub struct PickerProps {
    pub kind: TemporalKind,
    pub locale: Locale,
    /// 输入框里当前解析出的日期（高亮为选中）。
    pub selected: Option<NaiveDate>,
    pub today: NaiveDate,
    /// 正在显示的年月（已经按“跟随输入”解析过）。
    pub year: i32,
    pub month: u32,
    pub view: PickerView,
    /// 时间段输入框（日期列没有）；毫秒段只在列精度大于 0 时出现。
    pub time_inputs: Vec<(TimeField, Entity<InputState>)>,
    pub nullable: bool,
    /// 有可恢复的原值（编辑前的值）时才给“恢复原值”。
    pub restorable: bool,
}

/// 浏览位置落到具体年月：显式翻过页就用它，否则跟随输入的日期，再否则是今天。
pub fn resolve_month(cursor: PickerCursor, selected: Option<NaiveDate>, today: NaiveDate) -> (i32, u32) {
    cursor.month.unwrap_or_else(|| {
        let day = selected.unwrap_or(today);
        (day.year(), day.month())
    })
}

/// 在当前浏览位置上执行翻页 / 切换网格；返回新的位置。选日期、改时间等改写输入的操作不在这里处理。
pub fn navigate(cursor: PickerCursor, year: i32, month: u32, action: PickerAction) -> PickerCursor {
    let at = |year: i32, month: u32, view: PickerView| PickerCursor { view, month: Some((year, month)) };
    match (cursor.view, action) {
        (PickerView::Days, PickerAction::Prev) => {
            let (y, m) = temporal::shift_month(year, month, -1);
            at(y, m, PickerView::Days)
        }
        (PickerView::Days, PickerAction::Next) => {
            let (y, m) = temporal::shift_month(year, month, 1);
            at(y, m, PickerView::Days)
        }
        (PickerView::Months, PickerAction::Prev) => at(year - 1, month, PickerView::Months),
        (PickerView::Months, PickerAction::Next) => at(year + 1, month, PickerView::Months),
        (PickerView::Years, PickerAction::Prev) => at(year - 10, month, PickerView::Years),
        (PickerView::Years, PickerAction::Next) => at(year + 10, month, PickerView::Years),
        (_, PickerAction::ShowMonths) => at(year, month, PickerView::Months),
        (_, PickerAction::ShowYears) => at(year, month, PickerView::Years),
        (_, PickerAction::PickMonth(picked)) => at(year, picked, PickerView::Days),
        (_, PickerAction::PickYear(picked)) => at(picked, month, PickerView::Months),
        _ => cursor,
    }
}

/// 网格里的一格：日、月或年。
#[derive(Clone)]
struct GridCell {
    id: SharedString,
    label: SharedString,
    width: f32,
    selected: bool,
    today: bool,
    muted: bool,
    action: PickerAction,
}

fn cell_button(cell: GridCell, p: &Palette, on: PickerHandler) -> AnyElement {
    let GridCell { id, label, width, selected, today, muted, action } = cell;
    let accent = p.accent;
    let on_accent = p.on_accent;
    let text = if muted { p.text_faint } else { p.text };
    kit::interactive(
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .test_support()
            .w(px(width))
            .h(px(ControlSize::DATE_CELL))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(Radius::CONTROL))
            .text_size(px(TypeScale::SM))
            .text_color(text)
            .when(today && !selected, |this| this.border_1().border_color(p.accent).text_color(p.accent_text))
            .when(selected, |this| this.bg(accent).text_color(on_accent))
            .on_click(move |_, window, cx| on(action, window, cx))
            .child(label),
        p,
        false,
    )
    .into_any_element()
}

fn header(props: &PickerProps, p: &Palette, on: &PickerHandler) -> AnyElement {
    let locale = props.locale;
    let (title, title_action) = match props.view {
        PickerView::Days => (temporal::month_title(locale, props.year, props.month), PickerAction::ShowMonths),
        PickerView::Months => (match locale {
            Locale::ZhCn => format!("{}年", props.year),
            Locale::En => props.year.to_string(),
        }, PickerAction::ShowYears),
        PickerView::Years => {
            let start = temporal::decade_start(props.year) + 1;
            (format!("{start}–{}", start + 9), PickerAction::ShowYears)
        }
    };
    let prev = {
        let on = on.clone();
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| on(PickerAction::Prev, window, cx)
    };
    let next = {
        let on = on.clone();
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| on(PickerAction::Next, window, cx)
    };
    let title_on = on.clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(kit::icon_button("dt-prev", IconName::ChevronLeft, i18n::text(locale, "grid.temporal.prev"), prev).xsmall())
        .child(kit::interactive(
            div()
                .id("dt-title")
                .debug_selector(|| "dt-title".to_owned())
                .test_support()
                .px(px(Space::SM))
                .py(px(Space::XXS))
                .rounded(px(Radius::CONTROL))
                .text_size(px(TypeScale::SM))
                .text_color(p.text)
                .on_click(move |_, window, cx| title_on(title_action, window, cx))
                .child(SharedString::from(title)),
            p,
            false,
        ))
        .child(kit::icon_button("dt-next", IconName::ChevronRight, i18n::text(locale, "grid.temporal.next"), next).xsmall())
        .into_any_element()
}

fn days(props: &PickerProps, p: &Palette, on: &PickerHandler) -> AnyElement {
    let mut grid = div().id("dt-days").debug_selector(|| "dt-days".to_owned()).test_support().flex().flex_col();
    let mut week = div().flex();
    for label in temporal::weekday_labels(props.locale) {
        week = week.child(
            div()
                .w(px(ControlSize::DATE_CELL))
                .h(px(ControlSize::DATE_CELL))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(TypeScale::XS))
                .text_color(p.text_faint)
                .child(label),
        );
    }
    grid = grid.child(week);
    let cells = temporal::month_grid(props.year, props.month);
    for row in cells.chunks(7) {
        let mut line = div().flex();
        for day in row {
            let id = SharedString::from(format!("dt-day-{}", day.format("%Y-%m-%d")));
            let cell = GridCell {
                id,
                label: SharedString::from(day.day().to_string()),
                width: ControlSize::DATE_CELL,
                selected: props.selected == Some(*day),
                today: *day == props.today,
                muted: day.month() != props.month,
                action: PickerAction::PickDay(*day),
            };
            line = line.child(cell_button(cell, p, on.clone()));
        }
        grid = grid.child(line);
    }
    grid.into_any_element()
}

/// 月份或年份：4 行 × 3 列，宽度与日网格一致。
fn twelve(props: &PickerProps, p: &Palette, on: &PickerHandler) -> AnyElement {
    let width = ControlSize::DATE_CELL * 7. / 3.;
    let (id, cells): (&str, Vec<GridCell>) = match props.view {
        PickerView::Years => {
            let start = temporal::decade_start(props.year);
            let selected = props.selected.map(|day| day.year());
            ("dt-years", (start..start + 12).map(|year| GridCell {
                id: SharedString::from(format!("dt-year-{year}")),
                label: SharedString::from(year.to_string()),
                width,
                selected: selected == Some(year),
                today: year == props.today.year(),
                muted: year == start || year == start + 11,
                action: PickerAction::PickYear(year),
            }).collect())
        }
        _ => {
            let selected = props.selected.filter(|day| day.year() == props.year).map(|day| day.month());
            ("dt-months", (1..=12).map(|month| GridCell {
                id: SharedString::from(format!("dt-month-{month}")),
                label: SharedString::from(temporal::month_label(props.locale, month)),
                width,
                selected: selected == Some(month),
                today: props.today.year() == props.year && props.today.month() == month,
                muted: false,
                action: PickerAction::PickMonth(month),
            }).collect())
        }
    };
    let mut grid = div().id(id).debug_selector(move || id.to_owned()).test_support().flex().flex_col().gap(px(Space::XXS));
    for row in cells.chunks(3) {
        let mut line = div().flex();
        for cell in row.iter().cloned() {
            line = line.child(cell_button(cell, p, on.clone()));
        }
        grid = grid.child(line);
    }
    grid.into_any_element()
}

fn time_row(props: &PickerProps, p: &Palette, on: &PickerHandler) -> Option<AnyElement> {
    if props.time_inputs.is_empty() {
        return None;
    }
    let mut row = div()
        .id("dt-time")
        .debug_selector(|| "dt-time".to_owned())
        .test_support()
        .flex()
        .items_center()
        .gap(px(Space::XXS))
        .child(kit::icon(IconName::Clock, 13., p.text_faint));
    for (index, (field, input)) in props.time_inputs.iter().enumerate() {
        if index > 0 {
            let sep = if *field == TimeField::Fraction { "." } else { ":" };
            row = row.child(div().text_color(p.text_faint).text_size(px(TypeScale::SM)).child(sep));
        }
        let field = *field;
        let (name, width) = match field {
            TimeField::Hour => ("hour", ControlSize::TIME_SEGMENT_W),
            TimeField::Minute => ("minute", ControlSize::TIME_SEGMENT_W),
            TimeField::Second => ("second", ControlSize::TIME_SEGMENT_W),
            TimeField::Fraction => ("fraction", ControlSize::TIME_SEGMENT_W * 1.5),
        };
        let label = i18n::text(props.locale, match field {
            TimeField::Hour => "grid.temporal.hour",
            TimeField::Minute => "grid.temporal.minute",
            TimeField::Second => "grid.temporal.second",
            TimeField::Fraction => "grid.temporal.fraction",
        });
        let up = on.clone();
        let down = on.clone();
        let id = SharedString::from(format!("dt-{name}"));
        row = row.child(
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .test_support()
                .w(px(width))
                // 单行输入框把 ↑/↓ 当成移动光标吃掉：在外层捕获阶段先接住，改成调整这一段。
                .capture_action(move |_: &gpui_kit::base::input::MoveUp, window, cx| {
                    cx.stop_propagation();
                    up(PickerAction::Step(field, 1), window, cx);
                })
                .capture_action(move |_: &gpui_kit::base::input::MoveDown, window, cx| {
                    cx.stop_propagation();
                    down(PickerAction::Step(field, -1), window, cx);
                })
                .child(kit::text_input(SharedString::from(format!("dt-{name}-input")), input, label)),
        );
    }
    Some(row.into_any_element())
}

fn actions(props: &PickerProps, on: &PickerHandler) -> AnyElement {
    let locale = props.locale;
    let button = |id: &'static str, key: &str, action: PickerAction| {
        let on = on.clone();
        kit::tool_button(id, None, i18n::text(locale, key), move |_, window, cx| on(action, window, cx)).xsmall()
    };
    let mut row = div().id("dt-actions").debug_selector(|| "dt-actions".to_owned()).test_support().flex().flex_wrap().items_center().gap(px(Space::XS));
    if props.kind != TemporalKind::Date {
        row = row.child(button("dt-now", "grid.temporal.now", PickerAction::Now));
    }
    if props.kind != TemporalKind::Time {
        row = row.child(button("dt-today", "grid.temporal.today", PickerAction::Today));
    }
    if props.nullable {
        row = row.child(button("dt-null", "grid.value.setNull", PickerAction::Null));
    }
    if props.restorable {
        row = row.child(button("dt-restore", "grid.temporal.restore", PickerAction::Restore));
    }
    row.into_any_element()
}

/// 整个选择器：日历（TIME 列没有）、时间行、快捷操作。宽度固定为 7 格日期。
pub fn render(props: PickerProps, p: &Palette, on: PickerHandler) -> AnyElement {
    let mut root = div()
        .id("grid-edit-datetime")
        .debug_selector(|| "grid-edit-datetime".to_owned())
        .test_support()
        .flex()
        .flex_col()
        .gap(px(Space::SM))
        .w(px(ControlSize::DATE_CELL * 7.));
    if props.kind != TemporalKind::Time {
        root = root.child(header(&props, p, &on)).child(match props.view {
            PickerView::Days => days(&props, p, &on),
            _ => twelve(&props, p, &on),
        });
    }
    root.children(time_row(&props, p, &on)).child(actions(&props, &on)).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn cursor_follows_the_input_until_paged() {
        let today = day(2026, 11, 23);
        let follow = PickerCursor::default();
        assert_eq!(resolve_month(follow, Some(day(2024, 2, 29)), today), (2024, 2));
        assert_eq!(resolve_month(follow, None, today), (2026, 11));
        let next = navigate(follow, 2026, 12, PickerAction::Next);
        assert_eq!(next, PickerCursor { view: PickerView::Days, month: Some((2027, 1)) });
        assert_eq!(resolve_month(next, Some(day(2024, 2, 29)), today), (2027, 1));
    }

    #[test]
    fn title_switches_to_month_then_year_grid_and_back() {
        let start = PickerCursor::default();
        let months = navigate(start, 2026, 11, PickerAction::ShowMonths);
        assert_eq!(months.view, PickerView::Months);
        let years = navigate(months, 2026, 11, PickerAction::ShowYears);
        assert_eq!(years.view, PickerView::Years);
        // 年份网格十年一翻。
        let earlier = navigate(years, 2026, 11, PickerAction::Prev);
        assert_eq!(earlier.month, Some((2016, 11)));
        // 选年 → 月份网格，选月 → 日网格。
        let picked_year = navigate(earlier, 2016, 11, PickerAction::PickYear(2013));
        assert_eq!(picked_year, PickerCursor { view: PickerView::Months, month: Some((2013, 11)) });
        let picked_month = navigate(picked_year, 2013, 11, PickerAction::PickMonth(3));
        assert_eq!(picked_month, PickerCursor { view: PickerView::Days, month: Some((2013, 3)) });
        // 月份网格一翻一年。
        assert_eq!(navigate(months, 2026, 11, PickerAction::Next).month, Some((2027, 11)));
        // 改写输入的操作不动浏览位置。
        assert_eq!(navigate(months, 2026, 11, PickerAction::Now), months);
    }
}
