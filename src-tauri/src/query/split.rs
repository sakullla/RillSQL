//! 按引擎切分用户 SQL。只用于决定发送范围，不判断语句是否安全。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitReport {
    pub pieces: Vec<Piece>,
    pub notices: Vec<String>,
    pub refusal: Option<String>,
}

pub fn split_sql(engine: &str, sql: &str) -> SplitReport {
    let hash = hash_comments(engine);
    let mut notices = Vec::new();
    let mut pieces = Vec::new();
    let mut delimiter = ";".to_owned();
    let mut noticed = false;
    let mut i = 0usize;
    while i < sql.len() {
        let begin = skip_noise(sql, i, hash);
        if begin >= sql.len() {
            break;
        }
        if is_delimiter_command(sql, begin) {
            let line_end = eol_at(sql, begin);
            let arg = delimiter_argument(&sql[begin..line_end]);
            if engine == "postgres" || engine == "sqlite" {
                if !noticed {
                    notices.push(format!(
                        "{} 不使用 DELIMITER 这条客户端指令，已跳过。其余语句仍按该引擎执行。",
                        engine_label(engine)
                    ));
                    noticed = true;
                }
                i = skip_break(sql, line_end);
                continue;
            }
            let Some(arg) = arg else {
                return SplitReport {
                    pieces,
                    notices,
                    refusal: Some("自定义结束符没有写明，没有发送。".to_owned()),
                };
            };
            delimiter = arg;
            i = skip_break(sql, line_end);
            continue;
        }
        let scanned = scan_statement(sql, begin, &delimiter, hash);
        if let Some(piece) = scanned.piece {
            pieces.push(piece);
        }
        if let Some(refusal) = scanned.refusal {
            return SplitReport {
                pieces,
                notices,
                refusal: Some(refusal),
            };
        }
        if scanned.next <= begin {
            break;
        }
        i = scanned.next;
    }
    SplitReport {
        pieces,
        notices,
        refusal: None,
    }
}

pub fn first_keyword(sql: &str) -> String {
    let stripped = strip_comments(sql, false);
    let mut word = String::new();
    for ch in stripped.chars() {
        if ch.is_ascii_alphabetic() || ch == '_' {
            word.push(ch.to_ascii_uppercase());
        } else if word.is_empty() {
            continue;
        } else {
            break;
        }
    }
    word
}

pub fn is_write(sql: &str) -> bool {
    matches!(
        first_keyword(sql).as_str(),
        "INSERT"
            | "UPDATE"
            | "DELETE"
            | "CREATE"
            | "ALTER"
            | "DROP"
            | "TRUNCATE"
            | "REPLACE"
            | "MERGE"
            | "GRANT"
            | "REVOKE"
            | "CALL"
            | "DO"
            | "LOAD"
            | "COPY"
            | "VACUUM"
            | "REINDEX"
            | "ATTACH"
            | "DETACH"
    )
}

/// 顶层代码词及其字节区间。字符串、注释、dollar-quote 和括号内的词不出现。
pub fn top_level_words(engine: &str, sql: &str) -> (Vec<(String, usize, usize)>, usize) {
    let bytes = sql.as_bytes();
    let hash = hash_comments(engine);
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut dollar: Option<String> = None;
    let mut words = Vec::new();
    let mut end_of_code = 0usize;
    while i < bytes.len() {
        if let Some(tag) = dollar.clone() {
            let marker = format!("${tag}$");
            if sql[i..].starts_with(&marker) {
                end_of_code = i + marker.len();
                i += marker.len();
                dollar = None;
                continue;
            }
            i += utf8_len(bytes[i]);
            end_of_code = i;
            continue;
        }
        if let Some(q) = quote {
            if bytes[i] == q {
                if q == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    end_of_code = i;
                    continue;
                }
                quote = None;
                i += 1;
                end_of_code = i;
                continue;
            }
            if q == b'\'' && bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                end_of_code = i;
                continue;
            }
            i += utf8_len(bytes[i]);
            end_of_code = i;
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if hash && bytes[i] == b'#' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' || bytes[i] == b'`' {
            quote = Some(bytes[i]);
            i += 1;
            end_of_code = i;
            continue;
        }
        if bytes[i] == b'$' {
            if let Some(tag) = dollar_tag(sql, i) {
                dollar = Some(tag.clone());
                i += tag.len() + 2;
                end_of_code = i;
                continue;
            }
        }
        if bytes[i] == b'(' {
            depth += 1;
            i += 1;
            end_of_code = i;
            continue;
        }
        if bytes[i] == b')' {
            depth -= 1;
            i += 1;
            end_of_code = i;
            continue;
        }
        if depth == 0 && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            words.push((sql[start..i].to_ascii_uppercase(), start, i));
            end_of_code = i;
            continue;
        }
        i += utf8_len(bytes[i]);
        end_of_code = i;
    }
    (words, end_of_code)
}

#[cfg(test)]
pub fn statement_at(engine: &str, sql: &str, cursor: usize) -> Option<Piece> {
    let pieces = split_sql(engine, sql).pieces;
    if let Some(found) = pieces.iter().find(|piece| cursor >= piece.start && cursor <= piece.end) {
        return Some(found.clone());
    }
    pieces.into_iter().filter(|piece| piece.end <= cursor).next_back()
}

struct Scanned {
    piece: Option<Piece>,
    next: usize,
    refusal: Option<String>,
}

fn scan_statement(sql: &str, start: usize, delimiter: &str, hash: bool) -> Scanned {
    let bytes = sql.as_bytes();
    let mut i = start;
    let mut quote: Option<u8> = None;
    let mut dollar: Option<String> = None;
    let mut block = false;
    let mut paren = 0i32;
    let mut begin_depth = 0i32;
    let mut routine = false;
    let mut decided = false;
    let mut words: Vec<String> = Vec::new();
    let mut closed = false;
    let mut close_at = start;
    while i < bytes.len() {
        if let Some(tag) = dollar.clone() {
            let marker = format!("${tag}$");
            if sql[i..].starts_with(&marker) {
                i += marker.len();
                dollar = None;
                continue;
            }
            i += utf8_len(bytes[i]);
            continue;
        }
        if let Some(q) = quote {
            if bytes[i] == q {
                if q == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                quote = None;
            }
            i += 1;
            continue;
        }
        if block {
            if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block = false;
                i += 2;
                continue;
            }
            i += utf8_len(bytes[i]);
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i = eol_at(sql, i + 2);
            continue;
        }
        if hash && bytes[i] == b'#' {
            i = eol_at(sql, i + 1);
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            block = true;
            i += 2;
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' || bytes[i] == b'`' {
            quote = Some(bytes[i]);
            i += 1;
            continue;
        }
        if bytes[i] == b'$' {
            if paren == 0 && begin_depth == 0 && delimiter.starts_with('$') && sql[i..].starts_with(delimiter) {
                closed = true;
                close_at = i + delimiter.len();
                i = close_at;
                break;
            }
            if let Some(tag) = dollar_tag(sql, i) {
                dollar = Some(tag.clone());
                i += tag.len() + 2;
                continue;
            }
        }
        if bytes[i] == b'(' {
            paren += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            paren = paren.saturating_sub(1);
            i += 1;
            continue;
        }
        if paren == 0 && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let word = read_word(sql, i);
            let upper = word.0.to_ascii_uppercase();
            if !decided {
                words.push(upper.clone());
                match routine_look(&words) {
                    Look::Yes => {
                        routine = true;
                        decided = true;
                    }
                    Look::No => decided = true,
                    Look::Maybe => {}
                }
            }
            if upper == "BEGIN" && routine {
                begin_depth += 1;
            } else if upper == "END" && routine && begin_depth > 0 {
                let next = peek_word(sql, word.1, hash);
                if next.as_deref().map(|item| !is_routine_tail(item)).unwrap_or(true) {
                    begin_depth -= 1;
                }
            }
            i = word.1;
            continue;
        }
        if paren == 0 && begin_depth == 0 && !delimiter.is_empty() && sql[i..].starts_with(delimiter) {
            closed = true;
            close_at = i + delimiter.len();
            i = close_at;
            break;
        }
        i += utf8_len(bytes[i]);
    }
    if quote.is_some() {
        return refuse("引号没有闭合，没有发送。");
    }
    if block {
        return refuse("块注释没有闭合，没有发送。");
    }
    if dollar.is_some() {
        return refuse("dollar-quote 没有闭合，没有发送。");
    }
    if begin_depth > 0 {
        return refuse("例程体未闭合，没有发送。");
    }
    if !closed && delimiter != ";" && meaningful(&sql[start..i], hash) {
        return refuse("自定义结束符没有闭合，没有发送。");
    }
    let end = if closed { close_at } else { i };
    Scanned {
        piece: make_piece(sql, start, end, delimiter, closed, hash),
        next: end,
        refusal: None,
    }
}

fn refuse(reason: &str) -> Scanned {
    Scanned {
        piece: None,
        next: 0,
        refusal: Some(reason.to_owned()),
    }
}

fn make_piece(sql: &str, start: usize, end: usize, delimiter: &str, closed: bool, hash: bool) -> Option<Piece> {
    let body_end = if closed && delimiter != ";" {
        end.saturating_sub(delimiter.len())
    } else {
        end
    };
    let raw = &sql[start..body_end.max(start)];
    let lead = leading_noise(raw, hash);
    let body = raw[lead..].trim_end();
    if !meaningful(body, hash) {
        return None;
    }
    let text_start = start + lead;
    Some(Piece {
        text: body.to_owned(),
        start: text_start,
        end: text_start + body.len(),
    })
}

enum Look {
    Yes,
    No,
    Maybe,
}

fn routine_look(words: &[String]) -> Look {
    if words.first().map(String::as_str) != Some("CREATE") {
        return Look::No;
    }
    let mut index = 1usize;
    if words.get(index).map(String::as_str) == Some("OR") {
        match words.get(index + 1).map(String::as_str) {
            None => return Look::Maybe,
            Some("REPLACE") => index += 2,
            Some(_) => return Look::No,
        }
    }
    if words.get(index).map(String::as_str) == Some("DEFINER") {
        index += 1;
        if words.get(index).map(String::as_str) == Some("CURRENT_USER") {
            index += 1;
        }
    }
    match words.get(index).map(String::as_str) {
        None => Look::Maybe,
        Some("PROCEDURE" | "FUNCTION" | "TRIGGER") => Look::Yes,
        Some(_) => Look::No,
    }
}

fn is_routine_tail(word: &str) -> bool {
    matches!(word, "IF" | "LOOP" | "WHILE" | "REPEAT" | "CASE")
}

fn is_delimiter_command(sql: &str, index: usize) -> bool {
    read_word(sql, index).0.eq_ignore_ascii_case("DELIMITER")
}

fn delimiter_argument(command: &str) -> Option<String> {
    let rest = command.trim_start();
    let upper = rest.to_ascii_uppercase();
    if !upper.starts_with("DELIMITER") {
        return None;
    }
    let after = rest.get("DELIMITER".len()..)?;
    if after.starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    let arg = after.trim();
    if arg.is_empty() { None } else { Some(arg.to_owned()) }
}

fn engine_label(engine: &str) -> &'static str {
    match engine {
        "sqlite" => "SQLite",
        "mysql" => "MySQL",
        "mariadb" => "MariaDB",
        _ => "PostgreSQL",
    }
}

fn hash_comments(engine: &str) -> bool {
    engine == "mysql" || engine == "mariadb"
}

fn skip_noise(sql: &str, mut i: usize, hash: bool) -> usize {
    let bytes = sql.as_bytes();
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i = eol_at(sql, i + 2);
            continue;
        }
        if hash && bytes[i] == b'#' {
            i = eol_at(sql, i + 1);
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        break;
    }
    i
}

fn leading_noise(raw: &str, hash: bool) -> usize {
    skip_noise(raw, 0, hash)
}

fn meaningful(text: &str, hash: bool) -> bool {
    strip_comments(text, hash).chars().any(|ch| !ch.is_whitespace())
}

fn strip_comments(sql: &str, hash: bool) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        if let Some(q) = quote {
            out.push(bytes[i] as char);
            if bytes[i] == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            out.push(' ');
            i = eol_at(sql, i + 2);
            continue;
        }
        if hash && bytes[i] == b'#' {
            out.push(' ');
            i = eol_at(sql, i + 1);
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            out.push(' ');
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'\'' || bytes[i] == b'"' || bytes[i] == b'`' {
            quote = Some(bytes[i]);
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn peek_word(sql: &str, index: usize, hash: bool) -> Option<String> {
    let at = skip_noise(sql, index, hash);
    let bytes = sql.as_bytes();
    if at >= bytes.len() || !(bytes[at].is_ascii_alphabetic() || bytes[at] == b'_') {
        return None;
    }
    Some(read_word(sql, at).0.to_ascii_uppercase())
}

fn read_word(sql: &str, index: usize) -> (String, usize) {
    let bytes = sql.as_bytes();
    let mut end = index + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    (sql[index..end].to_owned(), end)
}

fn eol_at(sql: &str, index: usize) -> usize {
    let bytes = sql.as_bytes();
    let mut i = index;
    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
        i += 1;
    }
    i
}

fn skip_break(sql: &str, index: usize) -> usize {
    let bytes = sql.as_bytes();
    if index < bytes.len() && bytes[index] == b'\r' && index + 1 < bytes.len() && bytes[index + 1] == b'\n' {
        return index + 2;
    }
    if index < bytes.len() && (bytes[index] == b'\n' || bytes[index] == b'\r') {
        return index + 1;
    }
    index
}

fn dollar_tag(sql: &str, start: usize) -> Option<String> {
    let rest = sql.get(start + 1..)?;
    let end = rest.find('$')?;
    let tag = &rest[..end];
    if tag.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        Some(tag.to_owned())
    } else {
        None
    }
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}
