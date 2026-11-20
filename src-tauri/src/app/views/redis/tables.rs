//! 工作台里的只读表格（条目表、诊断页）与键名的实测截断。
//!
//! 表格统一用 `ui::read_table`：可以选中、⌘C 复制、右键复制、双击或回车发出主操作。
//! 每张表只在数据真的变化（按戳比较）时才换数据，渲染本身不触发重绘。

use std::hash::{Hash as _, Hasher as _};

use gpui_kit::component::table::TableEvent;
use gpui_kit::{App, Font};

use super::*;
use crate::ui::read_table::{ReadColumn, ReadRows, ReadTable, ReadTableEvent};

/// 条目表在 `tables` 里的键。
pub(super) const ENTRY_TABLE: &str = "entries";

pub(super) struct TableSlot {
    pub(super) table: Entity<ReadTable>,
    stamp: u64,
    _subscriptions: Vec<Subscription>,
}

/// 截断时保留尾部：放不下就用「…」加上能放下的最长后缀（键名的尾部最能区分同前缀的键）。
/// `measure` 返回一段文字的实际宽度；放得下时原样返回。
pub(crate) fn tail_fit(text: &str, available: f32, measure: impl Fn(&str) -> f32) -> String {
    if available <= 0. {
        return String::new();
    }
    if measure(text) <= available {
        return text.to_owned();
    }
    let starts: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
    // 二分找最小的起点：后缀越短越容易放下。
    let (mut lo, mut hi) = (1usize, starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        let candidate = format!("…{}", &text[starts[mid]..]);
        if measure(&candidate) <= available {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    if lo >= starts.len() {
        return "…".to_owned();
    }
    format!("…{}", &text[starts[lo]..])
}

impl RedisPane {
    /// 取（必要时新建）一张只读表；`stamp` 变了才换数据。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn table_slot(
        &mut self,
        key: &str,
        stamp: u64,
        build: impl FnOnce(&Self) -> (Vec<ReadColumn>, Vec<Vec<SharedString>>),
        empty: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ReadTable> {
        let stamp = {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            stamp.hash(&mut hasher);
            empty.hash(&mut hasher);
            matches!(self.locale, Locale::En).hash(&mut hasher);
            hasher.finish()
        };
        if let Some(slot) = self.tables.get(key) {
            if slot.stamp == stamp {
                return slot.table.clone();
            }
            let table = slot.table.clone();
            let (columns, rows) = build(self);
            let locale = self.locale;
            table.update(cx, |table, cx| {
                table.set_locale(locale, cx);
                table.set_data(columns, ReadRows::Static(rows), cx);
                table.set_empty_text(empty, cx);
            });
            if let Some(slot) = self.tables.get_mut(key) {
                slot.stamp = stamp;
            }
            return table;
        }
        let (columns, rows) = build(self);
        let locale = self.locale;
        let table = cx.new(|cx| {
            let mut table = ReadTable::new(columns, ReadRows::Static(rows), locale, window, cx);
            table.set_empty_text(empty, cx);
            table
        });
        let owned = key.to_owned();
        let mut subscriptions = vec![cx.subscribe_in(&table, window, move |this, _, event: &ReadTableEvent, window, cx| {
            if let ReadTableEvent::Activate(row) = event {
                this.table_activated(&owned, *row, window, cx);
            }
        })];
        if key == ENTRY_TABLE {
            let inner = table.read(cx).table().clone();
            subscriptions.push(cx.subscribe(&inner, |this, _, event: &TableEvent, cx| {
                let row = match event {
                    TableEvent::SelectCell(row, _) | TableEvent::SelectRow(row) => *row,
                    _ => return,
                };
                if let Some(index) = this.entry_rows.get(row).copied() {
                    if this.entry_focus != Some(index) {
                        this.entry_focus = Some(index);
                        cx.notify();
                    }
                }
            }));
        }
        self.tables.insert(key.to_owned(), TableSlot { table: table.clone(), stamp, _subscriptions: subscriptions });
        table
    }

    /// 在某张表里选中一整行（测试与键盘导航用）。
    #[cfg(test)]
    pub(super) fn select_table_row(&mut self, key: &str, row: usize, cx: &mut Context<Self>) {
        if let Some(slot) = self.tables.get(key) {
            slot.table.update(cx, |table, cx| table.select_row(row, false, cx));
        }
        cx.notify();
    }

    /// 表格当前选中（焦点）行。
    pub(super) fn table_focus_row(&self, key: &str, cx: &App) -> Option<usize> {
        let slot = self.tables.get(key)?;
        slot.table.read(cx).selection(cx).map(|selection| selection.focus.0)
    }

    /// 双击或回车：每张表的主操作。不触发任何破坏性操作。
    fn table_activated(&mut self, key: &str, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            ENTRY_TABLE => {
                let Some(index) = self.entry_rows.get(row).copied() else { return };
                self.entry_focus = Some(index);
                let id = match self.detail.as_deref().map(|detail| &detail.value) {
                    Some(RedisValue::Hash { .. }) => format!("redis-hash-edit:{index}"),
                    Some(RedisValue::List { .. }) => format!("redis-list-edit:{index}"),
                    Some(RedisValue::ZSet { .. }) => format!("redis-zset-edit:{index}"),
                    _ => format!("redis-entry-inspect:{index}"),
                };
                self.command(&id, cx);
            }
            "slowlog" => {
                if let Some(entry) = self.diag.slowlog.get(row) {
                    self.console_open = true;
                    self.command_input = entry.command.clone();
                    self.complete_open = false;
                    self.history_at = None;
                    self.focus_request = Some(Field::Command);
                    self.note = self.text("redis.slowlog.inserted");
                    cx.notify();
                }
            }
            "bigkeys" => self.command(&format!("redis-big-open:{row}"), cx),
            _ => {}
        }
        let _ = window;
    }

    /// 一段界面字体文字的实际宽度。
    pub(super) fn measure_ui(window: &Window, font: &Font, size: f32, text: &str) -> f32 {
        let run = gpui_kit::TextRun { len: text.len(), font: font.clone(), color: gpui_kit::black(), background_color: None, underline: None, strikethrough: None };
        f32::from(window.text_system().shape_line(SharedString::from(text.to_owned()), px(size), &[run], None).width)
    }

    /// 键名按实际宽度截断（保留尾部），结果按（键名, 宽度）缓存。
    pub(super) fn fit_key_name(&mut self, window: &Window, font: &Font, size: f32, text: &str, available: f32) -> String {
        let bucket = available.max(0.).round() as u32;
        if let Some(hit) = self.fit_cache.get(&(text.to_owned(), bucket)) {
            return hit.clone();
        }
        if self.fit_cache.len() > 4096 {
            self.fit_cache.clear();
        }
        let fitted = tail_fit(text, bucket as f32, |candidate| Self::measure_ui(window, font, size, candidate));
        self.fit_cache.insert((text.to_owned(), bucket), fitted.clone());
        fitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_fit_keeps_the_end_of_long_keys() {
        let measure = |text: &str| text.chars().count() as f32 * 7.;
        assert_eq!(tail_fit("user:1", 100., measure), "user:1", "放得下原样显示");
        let fitted = tail_fit("session:tenant-a:user:12345", 70., measure);
        assert!(fitted.starts_with('…') && "session:tenant-a:user:12345".ends_with(&fitted[3..]), "{fitted}");
        assert!(measure(&fitted) <= 70.);
        assert_eq!(fitted, "…ser:12345", "取能放下的最长后缀");
        assert_eq!(tail_fit("中文键名很长很长", 30., |text| text.chars().count() as f32 * 12.), "…长");
        assert_eq!(tail_fit("abc", 0., measure), "");
        assert_eq!(tail_fit("abcdef", 5., measure), "…");
    }
}
