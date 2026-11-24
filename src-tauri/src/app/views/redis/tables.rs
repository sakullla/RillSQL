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

/// 键名拆成“前缀 + 末段”：末段是最后一个分隔符之后的部分（通常是 ID，区分同前缀键的关键），
/// 前缀含末尾的分隔符。没有分隔符时前缀为空、整个键名就是末段。
pub(crate) fn split_key_name<'a>(name: &'a str, separators: &[String]) -> (&'a str, &'a str) {
    let segments = keyspace::split_segments(name, separators);
    if segments.len() <= 1 {
        return ("", name);
    }
    let last = segments.last().map(|segment| segment.text).unwrap_or(name);
    name.split_at(name.len() - last.len())
}

/// 键行的显示：前缀（弱化色，放不下时从左侧省略成「…尾部」）+ 末段（正常色，完整保留）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FittedKey {
    pub(crate) prefix: String,
    pub(crate) last: String,
}

/// 二分找最多保留的字数：`candidate(keep)` 放得下就再多留一些。
fn longest_fit(len: usize, room: f32, candidate: impl Fn(usize) -> String, measure: &impl Fn(&str) -> f32) -> String {
    let (mut lo, mut hi) = (0usize, len.saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&candidate(mid)) <= room {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let fitted = candidate(lo);
    if measure(&fitted) <= room { fitted } else { String::new() }
}

/// 按可用宽度排键名：放得下原样；放不下先从左侧省略前缀，末段保持完整；
/// 末段自己都放不下时前缀整个让出、末段尾部省略。`measure` 返回一段文字的实际宽度。
pub(crate) fn fit_key_parts(prefix: &str, last: &str, available: f32, measure: impl Fn(&str) -> f32) -> FittedKey {
    if available <= 0. {
        return FittedKey::default();
    }
    let last_w = measure(last);
    if measure(prefix) + last_w <= available {
        return FittedKey { prefix: prefix.to_owned(), last: last.to_owned() };
    }
    if last_w > available || prefix.is_empty() {
        let chars: Vec<char> = last.chars().collect();
        let last = longest_fit(chars.len(), available, |keep| chars[..keep].iter().collect::<String>() + "…", &measure);
        return FittedKey { prefix: String::new(), last };
    }
    let chars: Vec<char> = prefix.chars().collect();
    let prefix = longest_fit(chars.len(), available - last_w, |keep| "…".to_owned() + &chars[chars.len() - keep..].iter().collect::<String>(), &measure);
    FittedKey { prefix, last: last.to_owned() }
}

/// 键类型的固定宽度短标签（键行、模板行与详情头共用，颜色同类型色）。未知类型取前四个字母大写。
pub(crate) fn type_short_label(kind: &str) -> String {
    match kind {
        "string" => "STR".into(),
        "hash" => "HASH".into(),
        "list" => "LIST".into(),
        "set" => "SET".into(),
        "zset" => "ZSET".into(),
        "stream" => "STRM".into(),
        keys::JSON_TYPE | "json" => "JSON".into(),
        other => other.chars().take(4).collect::<String>().to_uppercase(),
    }
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
        self.table_slot_with(key, stamp, build, empty, false, window, cx)
    }

    /// 同 [`Self::table_slot`]；`keep_view` 时换数据保持滚动位置与选中（加载更多追加的一页）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn table_slot_with(
        &mut self,
        key: &str,
        stamp: u64,
        build: impl FnOnce(&Self) -> (Vec<ReadColumn>, Vec<Vec<SharedString>>),
        empty: Option<SharedString>,
        keep_view: bool,
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
                if keep_view {
                    table.set_data_keep_view(columns, ReadRows::Static(rows), cx);
                } else {
                    table.set_data(columns, ReadRows::Static(rows), cx);
                }
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
                if let Some(command) = self.diag.slowlog.get(row).map(|entry| entry.command.clone()) {
                    self.show_console();
                    self.command_input = command;
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

    /// 键名按实际宽度拆成前缀 + 末段（见 [`fit_key_parts`]），结果按（键名, 宽度）缓存。
    pub(super) fn fit_key_name(&mut self, window: &Window, font: &Font, size: f32, text: &str, available: f32) -> FittedKey {
        let bucket = available.max(0.).round() as u32;
        if self.fit_cache_seps != self.prefs.separators {
            self.fit_cache.clear();
            self.fit_cache_seps = self.prefs.separators.clone();
        }
        if let Some(hit) = self.fit_cache.get(&(text.to_owned(), bucket)) {
            return hit.clone();
        }
        if self.fit_cache.len() > 4096 {
            self.fit_cache.clear();
        }
        let (prefix, last) = split_key_name(text, &self.prefs.separators);
        let fitted = fit_key_parts(prefix, last, bucket as f32, |candidate| Self::measure_ui(window, font, size, candidate));
        self.fit_cache.insert((text.to_owned(), bucket), fitted.clone());
        fitted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_split_into_a_prefix_and_a_complete_last_segment() {
        let seps = vec![":".to_owned()];
        assert_eq!(split_key_name("a:b:c:12345", &seps), ("a:b:c:", "12345"));
        assert_eq!(split_key_name("plain", &seps), ("", "plain"));
        assert_eq!(split_key_name("trailing:", &seps), ("trailing:", ""));
        let multi = vec![":".to_owned(), "_".to_owned()];
        assert_eq!(split_key_name("cache_item:9582544418737", &multi), ("cache_item:", "9582544418737"));

        let measure = |text: &str| text.chars().count() as f32 * 7.;
        assert_eq!(fit_key_parts("a:b:c:", "12345", 200., measure), FittedKey { prefix: "a:b:c:".into(), last: "12345".into() }, "放得下原样显示");
        // 放不下：末段完整，前缀从左侧省略、保留靠近末段的部分。
        let prefix = "session:tenant-alpha:region-east:";
        let fitted = fit_key_parts(prefix, "12345", 140., measure);
        assert_eq!(fitted.last, "12345", "末段完整保留");
        let kept = fitted.prefix.strip_prefix('…').expect("前缀左侧省略");
        assert!(!kept.is_empty() && prefix.ends_with(kept), "{fitted:?}");
        assert!(measure(&fitted.prefix) + measure(&fitted.last) <= 140.);
        // 末段本身也放不下：前缀让出，末段尾部省略。
        let fitted = fit_key_parts("a:", "0123456789abcdef", 70., measure);
        assert_eq!(fitted.prefix, "");
        let head = fitted.last.strip_suffix('…').expect("末段尾部省略");
        assert!(!head.is_empty() && "0123456789abcdef".starts_with(head), "{fitted:?}");
        assert!(measure(&fitted.last) <= 70.);
        assert_eq!(fit_key_parts("a:", "b", 0., measure), FittedKey::default());
    }

    #[test]
    fn type_labels_are_short_and_fixed() {
        for (kind, label) in [("string", "STR"), ("hash", "HASH"), ("list", "LIST"), ("set", "SET"), ("zset", "ZSET"), ("stream", "STRM"), (keys::JSON_TYPE, "JSON"), ("module", "MODU"), ("", "")] {
            assert_eq!(type_short_label(kind), label, "{kind}");
        }
    }
}
