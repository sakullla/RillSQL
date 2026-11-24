//! 日期 / 时间列的编辑与查看辅助：识别列类型、宽松解析输入、按列的精度生成“现在”，
//! 以及悬停时给出星期与相对时间。纯函数，界面只负责把结果放进输入框。

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use crate::i18n::Locale;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporalKind {
    Date,
    Time,
    DateTime,
}

impl TemporalKind {
    /// 按声明类型判断。`timestamp with time zone`、`datetime2(3)` 这类带修饰的写法也认。
    pub fn of(type_name: &str) -> Option<Self> {
        let lower = type_name.trim().to_ascii_lowercase();
        let base = lower.split(['(', ' ']).next().unwrap_or("");
        match base {
            "date" => Some(Self::Date),
            "time" | "timetz" => Some(Self::Time),
            "timestamp" | "timestamptz" | "datetime" | "datetime2" | "smalldatetime" | "datetimeoffset" => Some(Self::DateTime),
            _ => None,
        }
    }

    pub fn format_hint(self) -> &'static str {
        match self {
            Self::Date => "YYYY-MM-DD",
            Self::Time => "HH:MM:SS",
            Self::DateTime => "YYYY-MM-DD HH:MM:SS",
        }
    }
}

/// 输入里时区后缀（Z、+08、+08:00、-0530）的位置；没有时区时为 None。
fn zone_suffix(text: &str) -> Option<usize> {
    if text.ends_with('Z') || text.ends_with('z') {
        return Some(text.len() - 1);
    }
    // 只在时间部分（已经出现过冒号）之后找 + / -，日期里的连字符不算。
    let time_start = text.find([' ', 'T', 't']).map(|at| at + 1).unwrap_or(0);
    let search = &text[time_start..];
    let at = search.rfind(['+', '-'])?;
    let tail = &search[at + 1..];
    if search[..at].contains(':') && !tail.is_empty() && tail.chars().all(|ch| ch.is_ascii_digit() || ch == ':') {
        Some(time_start + at)
    } else {
        None
    }
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    if text.len() == 8 && text.chars().all(|ch| ch.is_ascii_digit()) {
        return NaiveDate::parse_from_str(text, "%Y%m%d").ok();
    }
    let normalized: String = text.chars().map(|ch| if ch == '/' || ch == '.' { '-' } else { ch }).collect();
    NaiveDate::parse_from_str(&normalized, "%Y-%m-%d").ok()
}

fn parse_time(text: &str) -> Option<(NaiveTime, usize)> {
    let text = text.trim();
    let (clock, fraction) = match text.split_once('.') {
        Some((clock, fraction)) if !fraction.is_empty() && fraction.chars().all(|ch| ch.is_ascii_digit()) => (clock, fraction),
        Some(_) => return None,
        None => (text, ""),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|part| part.is_empty() || part.len() > 2 || !part.chars().all(|ch| ch.is_ascii_digit())) {
        return None;
    }
    let number = |index: usize| parts.get(index).map(|part| part.parse::<u32>().unwrap_or(99)).unwrap_or(0);
    let digits = fraction.len().min(9);
    let nanos = if digits == 0 { 0 } else { fraction[..digits].parse::<u32>().ok()? * 10u32.pow(9 - digits as u32) };
    NaiveTime::from_hms_nano_opt(number(0), number(1), number(2), nanos).map(|time| (time, digits))
}

fn format_time(time: NaiveTime, digits: usize) -> String {
    let base = time.format("%H:%M:%S").to_string();
    if digits == 0 {
        return base;
    }
    let fraction = format!("{:09}", time.nanosecond());
    format!("{base}.{}", &fraction[..digits.min(9)])
}

/// 输入的小数秒位数，用来让“现在”与已有值保持同样的精度。
pub fn fraction_digits(text: &str) -> usize {
    let text = text.trim();
    let clock = text.rsplit([' ', 'T']).next().unwrap_or(text);
    let clock = match zone_suffix(clock) {
        Some(at) => &clock[..at],
        None => clock,
    };
    clock.split_once('.').map(|(_, fraction)| fraction.chars().take_while(char::is_ascii_digit).count().min(9)).unwrap_or(0)
}

/// 宽松解析一段输入并规整成数据库能直接接受的写法：
/// `2023/7/4 9:05` → `2023-07-04 09:05:00`，`20230704` → `2023-07-04`，`2023-07-04T10:45:02Z` 保留时区。
/// 空白输入返回 None（交给调用方按空串/NULL 处理），解析不了返回 Err。
pub fn normalize(kind: TemporalKind, input: &str) -> Result<Option<String>, ()> {
    let text = input.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let (body, zone) = match kind {
        TemporalKind::Date => (text, ""),
        _ => match zone_suffix(text) {
            Some(at) => (&text[..at], &text[at..]),
            None => (text, ""),
        },
    };
    let body = body.trim();
    let out = match kind {
        TemporalKind::Date => {
            let date_part = body.split([' ', 'T']).next().unwrap_or(body);
            parse_date(date_part).ok_or(())?.format("%Y-%m-%d").to_string()
        }
        TemporalKind::Time => {
            let (time, digits) = parse_time(body).ok_or(())?;
            format_time(time, digits)
        }
        TemporalKind::DateTime => {
            let (date_part, time_part) = match body.split_once([' ', 'T', 't']) {
                Some((date, time)) => (date, time.trim()),
                None => (body, ""),
            };
            let date = parse_date(date_part).ok_or(())?;
            let (time, digits) = if time_part.is_empty() { (NaiveTime::MIN, 0) } else { parse_time(time_part).ok_or(())? };
            format!("{} {}", date.format("%Y-%m-%d"), format_time(time, digits))
        }
    };
    Ok(Some(format!("{out}{zone}")))
}

/// 读出输入里的日期（日历定位用）。
pub fn date_of(kind: TemporalKind, text: &str) -> Option<NaiveDate> {
    if kind == TemporalKind::Time {
        return None;
    }
    let body = text.trim();
    let date_part = body.split([' ', 'T']).next().unwrap_or(body);
    parse_date(date_part)
}

/// 列声明里的小数秒精度：`DATETIME(3)`、`timestamp(6) with time zone`、`TIME(3)`、`datetime2(7)`。
/// 没有声明 `(n)` 时返回 None，调用方退回已有值的小数位。DATE 列没有时间，精度恒为 0。
pub fn declared_precision(type_name: &str) -> Option<usize> {
    let kind = TemporalKind::of(type_name)?;
    if kind == TemporalKind::Date {
        return Some(0);
    }
    let lower = type_name.trim().to_ascii_lowercase();
    let open = lower.find('(')?;
    let close = lower[open..].find(')')? + open;
    lower[open + 1..close].trim().parse::<usize>().ok().map(|digits| digits.min(9))
}

/// 编辑时使用的小数秒位数：列声明优先，其次是已有值的位数。
pub fn precision(type_name: &str, sample: &str) -> usize {
    declared_precision(type_name).unwrap_or_else(|| fraction_digits(sample))
}

/// MySQL 的零日期与 PostgreSQL 的无穷值：不是日历上的某一天，但数据库接受，原样保存。
pub fn special_value(text: &str) -> bool {
    let text = text.trim().to_ascii_lowercase();
    matches!(text.as_str(), "infinity" | "-infinity" | "+infinity")
        || (text.starts_with("0000-00-00") && text.chars().all(|ch| ch == '0' || ch == '-' || ch == ':' || ch == ' ' || ch == '.'))
}

/// 日期时间值拆成的几段：选择器按段修改，再合回一段文本。时区后缀原样带着。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parts {
    pub date: Option<NaiveDate>,
    pub time: NaiveTime,
    pub zone: String,
}

/// 时间里可单独调整的一段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeField {
    Hour,
    Minute,
    Second,
    Fraction,
}

impl TimeField {
    pub const ALL: [TimeField; 4] = [TimeField::Hour, TimeField::Minute, TimeField::Second, TimeField::Fraction];

    /// 这一段在给定精度下的取值上限（不含）。
    pub fn modulus(self, digits: usize) -> u32 {
        match self {
            Self::Hour => 24,
            Self::Minute | Self::Second => 60,
            Self::Fraction => 10u32.pow(digits.clamp(1, 9) as u32),
        }
    }
}

/// 把输入拆开：能规整的输入才拆得开。空输入或认不出来时返回 None。
pub fn split(kind: TemporalKind, text: &str) -> Option<Parts> {
    let normalized = normalize(kind, text).ok().flatten()?;
    let (body, zone) = match kind {
        TemporalKind::Date => (normalized.as_str(), ""),
        _ => match zone_suffix(&normalized) {
            Some(at) => (&normalized[..at], &normalized[at..]),
            None => (normalized.as_str(), ""),
        },
    };
    let parts = match kind {
        TemporalKind::Date => Parts { date: Some(parse_date(body)?), time: NaiveTime::MIN, zone: String::new() },
        TemporalKind::Time => Parts { date: None, time: parse_time(body)?.0, zone: zone.to_owned() },
        TemporalKind::DateTime => {
            let (date, time) = body.split_once(' ')?;
            Parts { date: Some(parse_date(date)?), time: parse_time(time)?.0, zone: zone.to_owned() }
        }
    };
    Some(parts)
}

/// 按列类型与精度合成文本：DATE 只有日期，TIME 只有时间，小数秒严格按 `digits` 位。
pub fn compose(kind: TemporalKind, parts: &Parts, digits: usize) -> String {
    let date = parts.date.unwrap_or(NaiveDate::MIN).format("%Y-%m-%d").to_string();
    match kind {
        TemporalKind::Date => date,
        TemporalKind::Time => format!("{}{}", format_time(parts.time, digits), parts.zone),
        TemporalKind::DateTime => format!("{date} {}{}", format_time(parts.time, digits), parts.zone),
    }
}

/// 一段时间的当前数值；小数秒按 `digits` 位截取。
pub fn time_field(time: NaiveTime, field: TimeField, digits: usize) -> u32 {
    match field {
        TimeField::Hour => time.hour(),
        TimeField::Minute => time.minute(),
        TimeField::Second => time.second(),
        TimeField::Fraction => {
            let digits = digits.clamp(1, 9) as u32;
            time.nanosecond().min(999_999_999) / 10u32.pow(9 - digits)
        }
    }
}

/// 一段时间在输入框里的写法：时分秒两位，小数秒按位数补零。
pub fn field_text(time: NaiveTime, field: TimeField, digits: usize) -> String {
    let value = time_field(time, field, digits);
    match field {
        TimeField::Fraction => format!("{value:0width$}", width = digits.clamp(1, 9)),
        _ => format!("{value:02}"),
    }
}

/// 把一段改成 `value`（超出范围返回 None），其他段与日期不变。
pub fn set_time_field(time: NaiveTime, field: TimeField, value: u32, digits: usize) -> Option<NaiveTime> {
    if value >= field.modulus(digits) {
        return None;
    }
    match field {
        TimeField::Hour => time.with_hour(value),
        TimeField::Minute => time.with_minute(value),
        TimeField::Second => time.with_second(value),
        TimeField::Fraction => {
            let digits = digits.clamp(1, 9) as u32;
            time.with_nanosecond(value * 10u32.pow(9 - digits))
        }
    }
}

/// ↑/↓ 调整一段：在本段内循环（23 时再加一回到 0 时），不进位到其他段。
pub fn step_time_field(time: NaiveTime, field: TimeField, delta: i64, digits: usize) -> NaiveTime {
    let modulus = i64::from(field.modulus(digits));
    let current = i64::from(time_field(time, field, digits));
    let next = (current + delta).rem_euclid(modulus) as u32;
    set_time_field(time, field, next, digits).unwrap_or(time)
}

/// 周一起始的 6×7 月历：第一格是本月 1 日所在那周的周一，前后补齐相邻月份的日期。
pub fn month_grid(year: i32, month: u32) -> Vec<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(NaiveDate::MIN);
    let lead = i64::from(first.weekday().num_days_from_monday());
    let start = first - Duration::days(lead);
    (0..42).map(|offset| start + Duration::days(offset)).collect()
}

/// 按月平移年月（跨年进位）。
pub fn shift_month(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let index = year * 12 + month as i32 - 1 + delta;
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

/// 年份网格的起始年：十年一页，再补前后各一年凑满 12 格。
pub fn decade_start(year: i32) -> i32 {
    year.div_euclid(10) * 10 - 1
}

/// 周一起始的星期表头。
pub fn weekday_labels(locale: Locale) -> [&'static str; 7] {
    match locale {
        Locale::ZhCn => ["一", "二", "三", "四", "五", "六", "日"],
        Locale::En => ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"],
    }
}

/// 月份名：中文“11月”，英文缩写。
pub fn month_label(locale: Locale, month: u32) -> String {
    match locale {
        Locale::ZhCn => format!("{month}月"),
        Locale::En => ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
            .get(month.saturating_sub(1) as usize)
            .copied()
            .unwrap_or("")
            .to_owned(),
    }
}

/// 日历头标题：“2026年11月” / “Nov 2026”。
pub fn month_title(locale: Locale, year: i32, month: u32) -> String {
    match locale {
        Locale::ZhCn => format!("{year}年{month}月"),
        Locale::En => format!("{} {year}", month_label(locale, month)),
    }
}

fn weekday(locale: Locale, date: NaiveDate) -> &'static str {
    let index = date.weekday().num_days_from_monday() as usize;
    match locale {
        Locale::ZhCn => ["周一", "周二", "周三", "周四", "周五", "周六", "周日"][index],
        Locale::En => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][index],
    }
}

fn relative(locale: Locale, from: NaiveDateTime, now: NaiveDateTime) -> String {
    let seconds = (now - from).num_seconds();
    let future = seconds < 0;
    let abs = seconds.unsigned_abs();
    let (count, zh_unit, en_unit) = if abs < 60 {
        return match locale {
            Locale::ZhCn => "刚刚".to_owned(),
            Locale::En => "just now".to_owned(),
        };
    } else if abs < 3_600 {
        (abs / 60, "分钟", "minute")
    } else if abs < 86_400 {
        (abs / 3_600, "小时", "hour")
    } else if abs < 86_400 * 30 {
        (abs / 86_400, "天", "day")
    } else if abs < 86_400 * 365 {
        (abs / (86_400 * 30), "个月", "month")
    } else {
        (abs / (86_400 * 365), "年", "year")
    };
    match locale {
        Locale::ZhCn => format!("{count} {zh_unit}{}", if future { "后" } else { "前" }),
        Locale::En => {
            let plural = if count == 1 { "" } else { "s" };
            if future { format!("in {count} {en_unit}{plural}") } else { format!("{count} {en_unit}{plural} ago") }
        }
    }
}

/// 悬停说明：原值 · 星期 · 相对现在多久。解析不了（或纯时间）只给原值。
pub fn describe(kind: TemporalKind, text: &str, locale: Locale, now: NaiveDateTime) -> Option<String> {
    let date = date_of(kind, text)?;
    let moment = match kind {
        TemporalKind::Date => date.and_time(NaiveTime::MIN),
        _ => {
            let body = text.trim();
            let time = body
                .split_once([' ', 'T'])
                .map(|(_, rest)| match zone_suffix(rest) {
                    Some(at) => &rest[..at],
                    None => rest,
                })
                .and_then(|rest| parse_time(rest).map(|(time, _)| time))
                .unwrap_or(NaiveTime::MIN);
            date.and_time(time)
        }
    };
    let ago = if kind == TemporalKind::Date {
        let days = (now.date() - date).num_days();
        match (locale, days) {
            (Locale::ZhCn, 0) => "今天".to_owned(),
            (Locale::En, 0) => "today".to_owned(),
            (Locale::ZhCn, 1) => "昨天".to_owned(),
            (Locale::En, 1) => "yesterday".to_owned(),
            (Locale::ZhCn, -1) => "明天".to_owned(),
            (Locale::En, -1) => "tomorrow".to_owned(),
            _ => relative(locale, moment, now),
        }
    } else {
        relative(locale, moment, now)
    };
    Some(format!("{} · {} · {ago}", text.trim(), weekday(locale, date)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn kinds_follow_the_declared_type() {
        assert_eq!(TemporalKind::of("datetime"), Some(TemporalKind::DateTime));
        assert_eq!(TemporalKind::of("timestamp with time zone"), Some(TemporalKind::DateTime));
        assert_eq!(TemporalKind::of("DATETIME2(3)"), Some(TemporalKind::DateTime));
        assert_eq!(TemporalKind::of("date"), Some(TemporalKind::Date));
        assert_eq!(TemporalKind::of("time(6)"), Some(TemporalKind::Time));
        assert_eq!(TemporalKind::of("varchar(20)"), None);
        assert_eq!(TemporalKind::of("int"), None);
    }

    #[test]
    fn loose_input_is_normalized() {
        let dt = TemporalKind::DateTime;
        assert_eq!(normalize(dt, "2023/7/4 9:05"), Ok(Some("2023-07-04 09:05:00".to_owned())));
        assert_eq!(normalize(dt, "2023-07-04T10:45:02"), Ok(Some("2023-07-04 10:45:02".to_owned())));
        assert_eq!(normalize(dt, "2023-07-04"), Ok(Some("2023-07-04 00:00:00".to_owned())));
        assert_eq!(normalize(dt, "20230704"), Ok(Some("2023-07-04 00:00:00".to_owned())));
        assert_eq!(normalize(dt, "2023-07-04 10:45:02.120"), Ok(Some("2023-07-04 10:45:02.120".to_owned())));
        assert_eq!(normalize(dt, "2023-07-04 10:45:02+08:00"), Ok(Some("2023-07-04 10:45:02+08:00".to_owned())));
        assert_eq!(normalize(dt, "2023-07-04T10:45:02Z"), Ok(Some("2023-07-04 10:45:02Z".to_owned())));
        assert_eq!(normalize(dt, "  "), Ok(None));
        assert_eq!(normalize(dt, "2023-13-01"), Err(()));
        assert_eq!(normalize(dt, "2023-07-04 25:00"), Err(()));
        assert_eq!(normalize(dt, "昨天"), Err(()));
        assert_eq!(normalize(TemporalKind::Date, "2023.7.4"), Ok(Some("2023-07-04".to_owned())));
        assert_eq!(normalize(TemporalKind::Date, "2023-07-04 10:00:00"), Ok(Some("2023-07-04".to_owned())));
        assert_eq!(normalize(TemporalKind::Time, "9:5"), Ok(Some("09:05:00".to_owned())));
        assert_eq!(normalize(TemporalKind::Time, "09:05:00.5"), Ok(Some("09:05:00.5".to_owned())));
    }

    #[test]
    fn precision_comes_from_the_declared_type_then_the_value() {
        assert_eq!(declared_precision("DATETIME(3)"), Some(3));
        assert_eq!(declared_precision("timestamp(6) with time zone"), Some(6));
        assert_eq!(declared_precision("TIME(3)"), Some(3));
        assert_eq!(declared_precision("datetime2(12)"), Some(9));
        assert_eq!(declared_precision("date"), Some(0));
        assert_eq!(declared_precision("datetime"), None);
        assert_eq!(declared_precision("varchar(3)"), None);
        assert_eq!(precision("datetime", "2023-07-04 10:45:02.12"), 2);
        assert_eq!(precision("datetime(3)", "2023-07-04 10:45:02"), 3);
        assert!(special_value("0000-00-00 00:00:00"));
        assert!(special_value("infinity"));
        assert!(!special_value("2023-07-04"));
    }

    #[test]
    fn month_grid_starts_on_monday_and_spans_six_weeks() {
        let grid = month_grid(2026, 11);
        assert_eq!(grid.len(), 42);
        // 2026-11-01 是周日：第一格是 10 月 26 日（周一）。
        assert_eq!(grid[0], NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
        assert_eq!(grid[6], NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
        assert!(grid.iter().all(|day| day.weekday().num_days_from_monday() == (grid.iter().position(|d| d == day).unwrap() % 7) as u32));
        // 2024-04-01 本身是周一：第一格就是 1 日。
        assert_eq!(month_grid(2024, 4)[0], NaiveDate::from_ymd_opt(2024, 4, 1).unwrap());
        assert_eq!(shift_month(2026, 12, 1), (2027, 1));
        assert_eq!(shift_month(2026, 1, -1), (2025, 12));
        assert_eq!(shift_month(2026, 5, -12), (2025, 5));
        assert_eq!(decade_start(2026), 2019);
        assert_eq!(weekday_labels(Locale::ZhCn)[0], "一");
        assert_eq!(month_title(Locale::ZhCn, 2026, 11), "2026年11月");
        assert_eq!(month_title(Locale::En, 2026, 11), "Nov 2026");
    }

    #[test]
    fn split_and_compose_keep_the_other_half() {
        let dt = TemporalKind::DateTime;
        let parts = split(dt, "2026-11-23 10:45:02.123").unwrap();
        assert_eq!(parts.date, NaiveDate::from_ymd_opt(2026, 11, 23));
        assert_eq!(compose(dt, &parts, 3), "2026-11-23 10:45:02.123");
        // 精度按列：DATETIME(3) 补齐三位，DATETIME 去掉小数。
        let whole = split(dt, "2026-11-23 10:45:02").unwrap();
        assert_eq!(compose(dt, &whole, 3), "2026-11-23 10:45:02.000");
        assert_eq!(compose(dt, &parts, 0), "2026-11-23 10:45:02");
        // 跨年换日期：时间部分不变。
        let moved = Parts { date: NaiveDate::from_ymd_opt(2027, 1, 5), ..parts.clone() };
        assert_eq!(compose(dt, &moved, 3), "2027-01-05 10:45:02.123");
        // 调时、分：日期不变。
        let time = set_time_field(parts.time, TimeField::Hour, 8, 3).unwrap();
        let time = set_time_field(time, TimeField::Minute, 5, 3).unwrap();
        assert_eq!(compose(dt, &Parts { time, ..parts.clone() }, 3), "2026-11-23 08:05:02.123");
        assert_eq!(set_time_field(parts.time, TimeField::Hour, 24, 3), None);
        assert_eq!(time_field(parts.time, TimeField::Fraction, 3), 123);
        assert_eq!(field_text(parts.time, TimeField::Hour, 3), "10");
        assert_eq!(field_text(set_time_field(parts.time, TimeField::Fraction, 7, 3).unwrap(), TimeField::Fraction, 3), "007");
        let time = set_time_field(parts.time, TimeField::Fraction, 7, 3).unwrap();
        assert_eq!(compose(dt, &Parts { time, ..parts.clone() }, 3), "2026-11-23 10:45:02.007");
        // ↑/↓ 在本段内循环。
        let late = NaiveTime::from_hms_opt(23, 59, 0).unwrap();
        assert_eq!(step_time_field(late, TimeField::Hour, 1, 0).hour(), 0);
        assert_eq!(step_time_field(late, TimeField::Minute, 1, 0).minute(), 0);
        assert_eq!(step_time_field(late, TimeField::Minute, -60, 0).minute(), 59);
        // TIME 列只有时间，DATE 列只有日期；时区后缀原样保留。
        let clock = split(TemporalKind::Time, "9:5").unwrap();
        assert_eq!(clock.date, None);
        assert_eq!(compose(TemporalKind::Time, &clock, 3), "09:05:00.000");
        assert_eq!(compose(TemporalKind::Date, &split(TemporalKind::Date, "2026/1/2").unwrap(), 3), "2026-01-02");
        assert_eq!(compose(dt, &split(dt, "2026-11-23 10:00:00+08:00").unwrap(), 0), "2026-11-23 10:00:00+08:00");
        assert_eq!(split(dt, "不是日期"), None);
        assert_eq!(split(dt, ""), None);
    }

    #[test]
    fn hover_text_reads_weekday_and_age() {
        let now = at("2026-11-16 12:00:00");
        assert_eq!(describe(TemporalKind::DateTime, "2026-11-16 11:30:00", Locale::ZhCn, now).as_deref(), Some("2026-11-16 11:30:00 · 周一 · 30 分钟前"));
        assert_eq!(describe(TemporalKind::DateTime, "2023-07-04 10:45:02", Locale::En, now).as_deref(), Some("2023-07-04 10:45:02 · Tue · 3 years ago"));
        assert_eq!(describe(TemporalKind::Date, "2026-11-15", Locale::ZhCn, now).as_deref(), Some("2026-11-15 · 周日 · 昨天"));
        assert_eq!(describe(TemporalKind::DateTime, "2026-11-18 12:00:00", Locale::En, now).as_deref(), Some("2026-11-18 12:00:00 · Wed · in 2 days"));
        assert_eq!(describe(TemporalKind::Time, "10:00:00", Locale::ZhCn, now), None);
        assert_eq!(describe(TemporalKind::DateTime, "garbage", Locale::ZhCn, now), None);
    }
}
