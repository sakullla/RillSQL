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

/// “现在”：按列类型和已有值的小数秒精度生成本地时间文本。
pub fn now_text(kind: TemporalKind, sample: &str, now: NaiveDateTime) -> String {
    let digits = fraction_digits(sample);
    match kind {
        TemporalKind::Date => now.format("%Y-%m-%d").to_string(),
        TemporalKind::Time => format_time(now.time(), digits),
        TemporalKind::DateTime => format!("{} {}", now.format("%Y-%m-%d"), format_time(now.time(), digits)),
    }
}

/// 把日期部分换成 `date`，时间部分（含小数秒与时区）原样保留；纯日期列直接给日期。
pub fn with_date(kind: TemporalKind, current: &str, date: NaiveDate) -> String {
    let day = date.format("%Y-%m-%d").to_string();
    match kind {
        TemporalKind::Date => day,
        TemporalKind::Time => current.to_owned(),
        TemporalKind::DateTime => {
            let rest = current.trim().split_once([' ', 'T']).map(|(_, rest)| rest.trim().to_owned()).filter(|rest| !rest.is_empty());
            format!("{day} {}", rest.unwrap_or_else(|| "00:00:00".to_owned()))
        }
    }
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

/// 按天平移；时间部分保留。解析不了时返回 None。
pub fn shift_days(kind: TemporalKind, text: &str, days: i64) -> Option<String> {
    let date = date_of(kind, text)?;
    let moved = date.checked_add_signed(Duration::days(days))?;
    Some(with_date(kind, text, moved))
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
    fn now_keeps_the_column_precision() {
        let now = NaiveDateTime::parse_from_str("2026-11-16 19:40:26.123456", "%Y-%m-%d %H:%M:%S%.f").unwrap();
        assert_eq!(now_text(TemporalKind::DateTime, "2023-07-04 10:45:02", now), "2026-11-16 19:40:26");
        assert_eq!(now_text(TemporalKind::DateTime, "2023-07-04 10:45:02.000", now), "2026-11-16 19:40:26.123");
        assert_eq!(now_text(TemporalKind::Date, "", now), "2026-11-16");
        assert_eq!(now_text(TemporalKind::Time, "", now), "19:40:26");
    }

    #[test]
    fn picking_a_day_keeps_the_time() {
        let day = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        assert_eq!(with_date(TemporalKind::DateTime, "2023-07-04 10:45:02.5", day), "2024-02-29 10:45:02.5");
        assert_eq!(with_date(TemporalKind::DateTime, "", day), "2024-02-29 00:00:00");
        assert_eq!(with_date(TemporalKind::Date, "2023-07-04", day), "2024-02-29");
        assert_eq!(shift_days(TemporalKind::DateTime, "2023-07-04 10:45:02", 1).as_deref(), Some("2023-07-05 10:45:02"));
        assert_eq!(shift_days(TemporalKind::Date, "2024-03-01", -1).as_deref(), Some("2024-02-29"));
        assert_eq!(shift_days(TemporalKind::Date, "abc", 1), None);
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
