//! 连接/库对象概览（ADR-2 / R3）。
//!
//! 单击对象树里的连接、库或模式，主区打开一个“预览标签”列出下一层：连接级列库（名称、表数量、大小），
//! 库/模式级列表与视图（名称、估计行数、注释、数据+索引大小、更新时间）。这里只放纯状态与规则
//! （目标规格、过滤、排序、格式化），外壳负责排队读取与绘制；统计由 `catalog::read_object_stats` 读。

use std::cmp::Ordering;

use crate::catalog::model::{Engine, ObjectKind};
use crate::catalog::{stats_level, StatRow, StatsBody, StatsLevel};

/// 概览的范围：连接 | 连接名 | 库 | 模式 | （空，对象位）| 引擎 | 环境。
/// 与工作项规格同一布局，状态栏与关闭保护读到同一个连接与库。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewTarget {
    pub connection_id: String,
    pub connection_name: String,
    pub database: String,
    pub schema: String,
    pub engine: Engine,
    pub environment: String,
}

impl OverviewTarget {
    pub fn new(
        connection_id: impl Into<String>,
        connection_name: impl Into<String>,
        database: impl Into<String>,
        schema: impl Into<String>,
        engine: Engine,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            connection_id: connection_id.into(),
            connection_name: connection_name.into(),
            database: database.into(),
            schema: schema.into(),
            engine,
            environment: environment.into(),
        }
    }

    pub fn spec(&self) -> String {
        format!(
            "{}|{}|{}|{}||{}|{}",
            self.connection_id,
            self.connection_name,
            self.database,
            self.schema,
            self.engine.as_str(),
            self.environment
        )
    }

    pub fn parse(spec: &str) -> Option<Self> {
        let parts: Vec<&str> = spec.split('|').collect();
        if parts.len() < 6 {
            return None;
        }
        let part = |index: usize| parts.get(index).map(|text| text.trim()).unwrap_or_default().to_owned();
        let connection_id = part(0);
        if connection_id.is_empty() {
            return None;
        }
        let engine = Engine::parse(&part(5)).ok()?;
        Some(Self {
            connection_id,
            connection_name: part(1),
            database: part(2),
            schema: part(3),
            engine,
            environment: part(6),
        })
    }

    pub fn level(&self) -> StatsLevel {
        stats_level(self.engine, &self.database, &self.schema)
    }

    /// 标签标题：最内层的名字（模式 → 库 → 连接）。
    pub fn title(&self) -> String {
        [&self.schema, &self.database, &self.connection_name]
            .into_iter()
            .find(|text| !text.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.connection_id.clone())
    }

    /// 一行的下一层目标：库行进入库，模式行进入模式；对象行没有下一层。
    pub fn child(&self, row: &StatRow) -> Option<Self> {
        if row.kind.is_some() {
            return None;
        }
        let mut next = self.clone();
        match self.level() {
            StatsLevel::Databases => next.database = row.name.clone(),
            StatsLevel::Schemas => next.schema = row.name.clone(),
            StatsLevel::Objects => return None,
        }
        Some(next)
    }

    /// 对象所在的库：没有库层的引擎在连接级落到它的主库（与对象树节点同一个库名）。
    pub fn object_database(&self) -> String {
        if self.database.trim().is_empty() {
            crate::adapters::catalog_of(self.engine).default_database().unwrap_or_default().to_owned()
        } else {
            self.database.clone()
        }
    }

    /// 对象行的工作项规格（打开数据 / 结构用）。
    pub fn object_spec(&self, name: &str) -> String {
        crate::app::model::ObjectTarget::new(
            &self.connection_id,
            &self.connection_name,
            self.object_database(),
            &self.schema,
            name,
            self.engine.as_str(),
            &self.environment,
        )
        .spec()
    }
}

/// 概览的列。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OverviewColumn {
    Name,
    Tables,
    Rows,
    Comment,
    Size,
    Updated,
}

impl OverviewColumn {
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Name => "catalog.overview.name",
            Self::Tables => "catalog.overview.tables",
            Self::Rows => "catalog.overview.rows",
            Self::Comment => "catalog.overview.comment",
            Self::Size => "catalog.overview.size",
            Self::Updated => "catalog.overview.updated",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Tables => "tables",
            Self::Rows => "rows",
            Self::Comment => "comment",
            Self::Size => "size",
            Self::Updated => "updated",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        [Self::Name, Self::Tables, Self::Rows, Self::Comment, Self::Size, Self::Updated].into_iter().find(|column| column.id() == id)
    }

    /// 数字列右对齐，首次点击按从大到小排。
    pub fn numeric(self) -> bool {
        matches!(self, Self::Tables | Self::Rows | Self::Size)
    }

    /// 列宽权重（名称与注释吃剩余宽度）。
    pub fn width(self) -> f32 {
        match self {
            Self::Name => 2.2,
            Self::Comment => 2.4,
            Self::Updated => 1.4,
            Self::Tables | Self::Rows | Self::Size => 1.0,
        }
    }

    pub fn text(self, row: &StatRow) -> String {
        match self {
            Self::Name => row.name.clone(),
            Self::Tables => format_count(row.tables),
            Self::Rows => format_count(row.rows),
            Self::Comment => row.comment.clone(),
            Self::Size => format_size(row.size),
            Self::Updated => {
                if row.updated.is_empty() {
                    MISSING.to_owned()
                } else {
                    row.updated.clone()
                }
            }
        }
    }
}

/// 读不到的统计显示的占位。
pub const MISSING: &str = "—";

/// 这一层显示哪些列。
pub fn columns(level: StatsLevel) -> &'static [OverviewColumn] {
    match level {
        StatsLevel::Databases | StatsLevel::Schemas => &[OverviewColumn::Name, OverviewColumn::Tables, OverviewColumn::Size],
        StatsLevel::Objects => &[OverviewColumn::Name, OverviewColumn::Rows, OverviewColumn::Comment, OverviewColumn::Size, OverviewColumn::Updated],
    }
}

/// 字节数按 1024 进位显示一位小数；读不到为占位。
pub fn format_size(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else { return MISSING.to_owned() };
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 数量按千位分组；读不到为占位。
pub fn format_count(count: Option<u64>) -> String {
    let Some(count) = count else { return MISSING.to_owned() };
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 读取状态：待读、等连接、读取中、已读到、失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverviewLoad {
    /// 还没发起读取（新开、刷新或连接刚连上）。
    Pending,
    /// 连接还没连上：已按需排队连接，连上后再读。
    Connecting,
    Loading,
    Ready,
    Failed(String),
}

/// 一个概览标签的状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewState {
    pub target: OverviewTarget,
    pub load: OverviewLoad,
    pub rows: Vec<StatRow>,
    /// 统计读不到时的原因；rows 此时只有名称。
    pub missing: Option<String>,
    pub filter: String,
    pub sort: OverviewColumn,
    pub descending: bool,
    /// 选中的行（按名字，过滤与排序后仍指向同一行）。
    pub selected: Option<String>,
    /// 读取代数：刷新后迟到的旧结果丢弃。
    pub generation: u64,
    /// 失败来自按需连接（不是统计读取）：连接之后从别处连上（密码框、对象树）时自动重读。
    pub connect_failed: bool,
}

impl OverviewState {
    pub fn new(target: OverviewTarget) -> Self {
        Self {
            target,
            load: OverviewLoad::Pending,
            rows: Vec::new(),
            missing: None,
            filter: String::new(),
            sort: OverviewColumn::Name,
            descending: false,
            selected: None,
            generation: 0,
            connect_failed: false,
        }
    }

    pub fn level(&self) -> StatsLevel {
        self.target.level()
    }

    /// 开始一次读取，返回这次读取的代数。
    pub fn begin_load(&mut self) -> u64 {
        self.generation += 1;
        self.load = OverviewLoad::Loading;
        self.generation
    }

    /// 读取结果回填；代数不一致（期间刷新过）就丢弃。
    pub fn finish_load(&mut self, generation: u64, result: Result<StatsBody, String>) -> bool {
        if generation != self.generation || self.load != OverviewLoad::Loading {
            return false;
        }
        match result {
            Ok(body) => {
                self.rows = body.rows;
                self.missing = body.missing;
                self.load = OverviewLoad::Ready;
                if self.selected.as_ref().is_some_and(|name| !self.rows.iter().any(|row| &row.name == name)) {
                    self.selected = None;
                }
            }
            Err(error) => {
                self.rows.clear();
                self.missing = None;
                self.load = OverviewLoad::Failed(error);
            }
        }
        true
    }

    /// 刷新：保留过滤、排序与选择，重新排队读取；进行中的旧读取作废。
    pub fn reload(&mut self) {
        self.generation += 1;
        self.load = OverviewLoad::Pending;
        self.connect_failed = false;
    }

    /// 点击列头：同一列切换升降序；换列时数字列先从大到小、文本列先从 A 到 Z。
    pub fn toggle_sort(&mut self, column: OverviewColumn) {
        if self.sort == column {
            self.descending = !self.descending;
        } else {
            self.sort = column;
            self.descending = column.numeric();
        }
    }

    /// 过滤（名称或注释包含关键字，不分大小写）并排序后的行。读不到统计的行排在最后。
    pub fn visible(&self) -> Vec<&StatRow> {
        let needle = self.filter.trim().to_lowercase();
        let mut rows: Vec<&StatRow> = self
            .rows
            .iter()
            .filter(|row| needle.is_empty() || row.name.to_lowercase().contains(&needle) || row.comment.to_lowercase().contains(&needle))
            .collect();
        let column = self.sort;
        let descending = self.descending;
        rows.sort_by(|left, right| {
            let primary = match column {
                OverviewColumn::Name => Some(natural(&left.name, &right.name)),
                OverviewColumn::Comment => known(text_key(&left.comment), text_key(&right.comment)),
                OverviewColumn::Updated => known(text_key(&left.updated), text_key(&right.updated)),
                OverviewColumn::Tables => known(left.tables, right.tables),
                OverviewColumn::Rows => known(left.rows, right.rows),
                OverviewColumn::Size => known(left.size, right.size),
            };
            match primary {
                Some(order) => (if descending { order.reverse() } else { order }).then_with(|| natural(&left.name, &right.name)),
                // 未知值不随升降序翻转，始终在已知值之后。
                None => unknown_last(column, left, right).then_with(|| natural(&left.name, &right.name)),
            }
        });
        rows
    }

    /// 选中行的下一个/上一个（键盘上下移动）。
    pub fn step_selection(&mut self, delta: isize) {
        let names: Vec<String> = self.visible().iter().map(|row| row.name.clone()).collect();
        if names.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.selected.as_ref().and_then(|name| names.iter().position(|item| item == name));
        let next = match current {
            Some(index) => (index as isize + delta).clamp(0, names.len() as isize - 1) as usize,
            None if delta < 0 => names.len() - 1,
            None => 0,
        };
        self.selected = Some(names[next].clone());
    }

    pub fn row(&self, name: &str) -> Option<&StatRow> {
        self.rows.iter().find(|row| row.name == name)
    }

    /// 已知大小的合计；一个都读不到时为 None。
    pub fn total_size(&self) -> Option<u64> {
        let sizes: Vec<u64> = self.rows.iter().filter_map(|row| row.size).collect();
        (!sizes.is_empty()).then(|| sizes.iter().sum())
    }

    /// 对象行是否能打开数据（表与视图）。
    pub fn opens_data(row: &StatRow) -> bool {
        matches!(row.kind, Some(ObjectKind::Table | ObjectKind::View))
    }
}

fn text_key(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text != MISSING).then(|| text.to_lowercase())
}

/// 两边都已知时给出顺序；任一边未知时返回 None，交给 `unknown_last`。
fn known<T: Ord>(left: Option<T>, right: Option<T>) -> Option<Ordering> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.cmp(&right)),
        _ => None,
    }
}

fn unknown_last(column: OverviewColumn, left: &StatRow, right: &StatRow) -> Ordering {
    let present = |row: &StatRow| match column {
        OverviewColumn::Tables => row.tables.is_some(),
        OverviewColumn::Rows => row.rows.is_some(),
        OverviewColumn::Size => row.size.is_some(),
        OverviewColumn::Comment => text_key(&row.comment).is_some(),
        OverviewColumn::Updated => text_key(&row.updated).is_some(),
        OverviewColumn::Name => true,
    };
    present(right).cmp(&present(left))
}

/// 名称排序：不分大小写，数字段按数值（t2 在 t10 之前）。
fn natural(left: &str, right: &str) -> Ordering {
    let mut a = left.chars().peekable();
    let mut b = right.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return left.cmp(right),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut left_digits = String::new();
                while let Some(ch) = a.peek().copied().filter(char::is_ascii_digit) {
                    left_digits.push(ch);
                    a.next();
                }
                let mut right_digits = String::new();
                while let Some(ch) = b.peek().copied().filter(char::is_ascii_digit) {
                    right_digits.push(ch);
                    b.next();
                }
                let left_trim = left_digits.trim_start_matches('0');
                let right_trim = right_digits.trim_start_matches('0');
                let order = left_trim.len().cmp(&right_trim.len()).then_with(|| left_trim.cmp(right_trim));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests;
