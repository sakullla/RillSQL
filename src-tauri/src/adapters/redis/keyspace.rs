//! 键空间的本地组织（纯逻辑）：按连接配置的分隔符拆段、命名空间树分组与计数、
//! 变量段识别与模板归并（`order:1001`、`order:<uuid>` → `order:*`）、用户规则覆盖。
//! 只处理内存键索引里已读到的键，不发网络请求。

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

/// 默认分隔符。
pub const DEFAULT_SEPARATOR: &str = ":";
/// 后台扫描的默认 COUNT 提示。
pub const DEFAULT_SCAN_COUNT: usize = 500;
/// 内存键索引的默认上限：超过即停止扫描并提示改用模式搜索。
pub const DEFAULT_SCAN_CAP: usize = 200_000;
/// 模板内存估算的默认抽样数。
pub const DEFAULT_MEMORY_SAMPLES: usize = 20;
/// 模板里变量段的显示。
pub const VARIABLE: &str = "*";

/// 用户对模板某一段的标记：`pattern` 是把该段写成 `*` 后的模板（其余段按自动识别后的形态），
/// `index` 为段下标，`variable` 为该段是否视为变量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRule {
    pub pattern: String,
    pub index: usize,
    pub variable: bool,
}

/// 值区条目表宽度的保存范围（像素）。
pub const VALUE_SPLIT_MIN: u32 = 200;
pub const VALUE_SPLIT_MAX: u32 = 1600;

/// 按连接保存的键空间偏好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyspacePrefs {
    pub separators: Vec<String>,
    pub rules: Vec<TemplateRule>,
    pub scan_count: usize,
    pub scan_cap: usize,
    pub memory_samples: usize,
    /// 值区左栏（条目表）的宽度，像素；0 表示按默认比例。分隔条拖动后保存。
    pub value_split: u32,
    /// 集合类键右侧的值面板是否收起。
    pub inspector_hidden: bool,
}

impl Default for KeyspacePrefs {
    fn default() -> Self {
        Self {
            separators: vec![DEFAULT_SEPARATOR.to_owned()],
            rules: Vec::new(),
            scan_count: DEFAULT_SCAN_COUNT,
            scan_cap: DEFAULT_SCAN_CAP,
            memory_samples: DEFAULT_MEMORY_SAMPLES,
            value_split: 0,
            inspector_hidden: false,
        }
    }
}

impl KeyspacePrefs {
    /// 偏好命名空间（连接偏好表里的键）。
    pub const NS: &'static str = "keyspace";

    pub fn from_json(text: Option<&str>) -> Self {
        let mut prefs: Self = text.and_then(|text| serde_json::from_str(text).ok()).unwrap_or_default();
        prefs.normalize();
        prefs
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// 去空、去重；全部为空时回到默认 `:`。数值钳到合理范围。
    pub fn normalize(&mut self) {
        let mut seen = BTreeSet::new();
        self.separators.retain(|sep| !sep.is_empty() && seen.insert(sep.clone()));
        if self.separators.is_empty() {
            self.separators.push(DEFAULT_SEPARATOR.to_owned());
        }
        self.scan_count = self.scan_count.clamp(10, 10_000);
        self.scan_cap = self.scan_cap.clamp(1_000, 5_000_000);
        self.memory_samples = self.memory_samples.clamp(1, 500);
        if self.value_split != 0 {
            self.value_split = self.value_split.clamp(VALUE_SPLIT_MIN, VALUE_SPLIT_MAX);
        }
    }

    /// 分隔符输入框的文本：空格分开（`:` 与 `|` → `: |`）。
    pub fn separators_text(&self) -> String {
        self.separators.join(" ")
    }

    pub fn set_separators_text(&mut self, text: &str) {
        self.separators = text.split_whitespace().map(str::to_owned).collect();
        self.normalize();
    }
}

/// 一个键拆出的段：段文本与其后的分隔符（最后一段没有）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment<'a> {
    pub text: &'a str,
    pub sep: &'a str,
}

/// 按分隔符拆段；多个分隔符时同一位置优先匹配更长的。
pub fn split_segments<'a>(key: &'a str, separators: &[String]) -> Vec<Segment<'a>> {
    let mut seps: Vec<&str> = separators.iter().map(String::as_str).filter(|sep| !sep.is_empty()).collect();
    seps.sort_by_key(|sep| std::cmp::Reverse(sep.len()));
    let mut out = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let bytes = key.as_bytes();
    while at < bytes.len() {
        if let Some(sep) = seps.iter().find(|sep| key[at..].starts_with(**sep)) {
            out.push(Segment { text: &key[start..at], sep: &key[at..at + sep.len()] });
            at += sep.len();
            start = at;
            continue;
        }
        at += key[at..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    out.push(Segment { text: &key[start..], sep: "" });
    out
}

/// 命名空间树的一行：文件夹（前缀）或叶子键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    Folder { prefix: String, segment: String, depth: usize, count: usize, open: bool },
    Key { index: usize, display: String, depth: usize },
}

/// 按分隔符把键组织成树：文件夹显示其下键数；收起的文件夹不展开子行。`names` 为 (索引, 键名)。
pub fn build_tree(names: &[(usize, &str)], separators: &[String], expanded: &BTreeSet<String>) -> Vec<TreeRow> {
    let mut sorted: Vec<(usize, &str)> = names.to_vec();
    sorted.sort_by(|a, b| a.1.cmp(b.1));
    // 每个前缀下的键数（前缀为原文前缀，不含末尾分隔符）。
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut splits: Vec<Vec<Segment>> = Vec::with_capacity(sorted.len());
    for (_, name) in &sorted {
        let segments = split_segments(name, separators);
        let mut end = 0;
        for segment in &segments[..segments.len().saturating_sub(1)] {
            end += segment.text.len();
            *counts.entry(&name[..end]).or_insert(0) += 1;
            end += segment.sep.len();
        }
        splits.push(segments);
    }
    let mut rows = Vec::new();
    let mut emitted: BTreeSet<&str> = BTreeSet::new();
    for ((index, name), segments) in sorted.iter().zip(splits.iter()) {
        if segments.len() < 2 {
            rows.push(TreeRow::Key { index: *index, display: (*name).to_owned(), depth: 0 });
            continue;
        }
        let mut end = 0;
        let mut visible = true;
        for (depth, segment) in segments[..segments.len() - 1].iter().enumerate() {
            end += segment.text.len();
            let prefix = &name[..end];
            let open = expanded.contains(prefix);
            if visible && emitted.insert(prefix) {
                rows.push(TreeRow::Folder {
                    prefix: prefix.to_owned(),
                    segment: segment.text.to_owned(),
                    depth,
                    count: counts.get(prefix).copied().unwrap_or(0),
                    open,
                });
            }
            visible &= open;
            end += segment.sep.len();
        }
        if visible {
            let leaf = segments.last().map(|segment| segment.text).unwrap_or(name);
            let display = if leaf.is_empty() { (*name).to_owned() } else { leaf.to_owned() };
            rows.push(TreeRow::Key { index: *index, display, depth: segments.len() - 1 });
        }
    }
    rows
}

/// 文件夹前缀对应的 SCAN MATCH 模式：前缀 + 第一个分隔符 + `*`（前缀里的 glob 字符先转义）。
pub fn folder_pattern(prefix: &str, separators: &[String]) -> String {
    let sep = separators.first().map(String::as_str).unwrap_or(DEFAULT_SEPARATOR);
    format!("{}{}*", glob_escape(prefix), glob_escape(sep))
}

/// SCAN MATCH 的 glob 转义。
pub fn glob_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// 段是否像变量：纯数字、UUID、≥16 位十六进制、日期/时间戳样式、含数字的长随机串。
pub fn is_variable_segment(segment: &str) -> bool {
    let text = segment.trim();
    if text.is_empty() {
        return false;
    }
    let len = text.chars().count();
    if text.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if is_uuid(text) {
        return true;
    }
    if len >= 16 && text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return true;
    }
    if looks_like_date(text) {
        return true;
    }
    // 含数字的长随机串：≥12 位，字母数字（可带 - _），数字至少两个且字母数字混排。
    let digits = text.chars().filter(char::is_ascii_digit).count();
    let letters = text.chars().filter(char::is_ascii_alphabetic).count();
    len >= 12 && digits >= 2 && letters >= 1 && text.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    (groups.len() == 5 && groups.iter().zip(lens).all(|(group, len)| group.len() == len && group.chars().all(|ch| ch.is_ascii_hexdigit())))
        || (text.len() == 32 && text.chars().all(|ch| ch.is_ascii_hexdigit()))
}

/// 2024-01-02、2024/01/02、20240102、2024-01-02T10:00:00Z、10:00:00 之类。
fn looks_like_date(text: &str) -> bool {
    let digits = text.chars().filter(char::is_ascii_digit).count();
    text.chars().next().is_some_and(|ch| ch.is_ascii_digit())
        && digits >= 6
        && text.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '-' | '/' | ':' | '.' | 'T' | 'Z' | '+' | ' '))
}

/// 键的模板：自动识别变量段后，按用户规则覆盖，变量段写成 `*`，分隔符原样保留。
pub fn template_of(key: &str, separators: &[String], rules: &[TemplateRule]) -> String {
    let segments = split_segments(key, separators);
    let mut variable: Vec<bool> = segments.iter().map(|segment| is_variable_segment(segment.text)).collect();
    // 只有一段（没有分隔符）的键不做归并：否则所有数字键都会被并成一个 `*`。
    if segments.len() < 2 {
        variable = vec![false];
    }
    for rule in rules {
        if rule.index >= segments.len() {
            continue;
        }
        let shape = render(&segments, &variable, Some(rule.index));
        if shape == rule.pattern {
            variable[rule.index] = rule.variable;
        }
    }
    render(&segments, &variable, None)
}

/// 把段按变量标记拼回模板；`force` 位置强制写成 `*`（用于规则匹配的形态）。
fn render(segments: &[Segment], variable: &[bool], force: Option<usize>) -> String {
    let mut out = String::new();
    for (index, segment) in segments.iter().enumerate() {
        if variable[index] || force == Some(index) {
            out.push_str(VARIABLE);
        } else {
            out.push_str(segment.text);
        }
        out.push_str(segment.sep);
    }
    out
}

/// 在模板上标记某一段为变量/固定，生成要保存的规则。`sample` 是该模板下的任一键（取固定段的原文）。
pub fn rule_for(sample: &str, separators: &[String], rules: &[TemplateRule], index: usize, variable: bool) -> Option<TemplateRule> {
    let segments = split_segments(sample, separators);
    if index >= segments.len() || segments.len() < 2 {
        return None;
    }
    let mut flags: Vec<bool> = segments.iter().map(|segment| is_variable_segment(segment.text)).collect();
    for rule in rules {
        if rule.index < segments.len() && render(&segments, &flags, Some(rule.index)) == rule.pattern {
            flags[rule.index] = rule.variable;
        }
    }
    Some(TemplateRule { pattern: render(&segments, &flags, Some(index)), index, variable })
}

/// 规则表里加入/替换一条（同形态同下标的旧规则被替换）。
pub fn upsert_rule(rules: &mut Vec<TemplateRule>, rule: TemplateRule) {
    rules.retain(|item| !(item.pattern == rule.pattern && item.index == rule.index));
    rules.push(rule);
}

/// 模板的分段（展示与标记用）：(段文本, 是否变量)。
pub fn template_segments(template: &str, separators: &[String]) -> Vec<(String, bool)> {
    split_segments(template, separators).into_iter().map(|segment| (segment.text.to_owned(), segment.text == VARIABLE)).collect()
}

/// 模板对应的 SCAN MATCH 模式：固定段转义，变量段为 `*`。
pub fn template_pattern(template: &str, separators: &[String]) -> String {
    split_segments(template, separators)
        .into_iter()
        .map(|segment| {
            let text = if segment.text == VARIABLE { VARIABLE.to_owned() } else { glob_escape(segment.text) };
            format!("{text}{}", glob_escape(segment.sep))
        })
        .collect()
}

/// 一个模板分组：键数、样例键索引、各类型计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateGroup {
    pub template: String,
    pub count: usize,
    pub members: Vec<usize>,
    pub types: Vec<(String, usize)>,
}

/// 按模板归并：`keys` 为 (索引, 键名, 类型)。按键数降序、模板名升序。
pub fn group_templates(keys: &[(usize, &str, &str)], separators: &[String], rules: &[TemplateRule]) -> Vec<TemplateGroup> {
    let mut groups: HashMap<String, (Vec<usize>, HashMap<String, usize>)> = HashMap::new();
    for (index, name, key_type) in keys {
        let template = template_of(name, separators, rules);
        let entry = groups.entry(template).or_default();
        entry.0.push(*index);
        *entry.1.entry((*key_type).to_owned()).or_insert(0) += 1;
    }
    let mut out: Vec<TemplateGroup> = groups
        .into_iter()
        .map(|(template, (members, types))| {
            let mut types: Vec<(String, usize)> = types.into_iter().collect();
            types.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            TemplateGroup { template, count: members.len(), members, types }
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.template.cmp(&b.template)));
    out
}

/// 抽样：均匀取至多 `n` 个（保持顺序），用于内存估算与预览样例。
pub fn sample<T: Clone>(items: &[T], n: usize) -> Vec<T> {
    if n == 0 || items.is_empty() {
        return Vec::new();
    }
    if items.len() <= n {
        return items.to_vec();
    }
    let step = items.len() as f64 / n as f64;
    (0..n).map(|i| items[((i as f64) * step) as usize].clone()).collect()
}

/// 模板内存估算：样本平均 × 键数。
pub fn estimate_memory(samples: &[i64], count: usize) -> Option<u64> {
    let valid: Vec<i64> = samples.iter().copied().filter(|bytes| *bytes >= 0).collect();
    if valid.is_empty() {
        return None;
    }
    let avg = valid.iter().sum::<i64>() as f64 / valid.len() as f64;
    Some((avg * count as f64).round() as u64)
}

/// 字节数的紧凑文本：`512 B`、`1.5 KB`、`3.2 MB`。
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
