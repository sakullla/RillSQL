//! 按语句切分用户 SQL。只用于决定发送范围，不判断语句是否安全。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

pub fn split_sql(sql: &str) -> Vec<Piece> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut quote: Option<u8> = None;
    let mut dollar: Option<String> = None;
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
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
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
            continue;
        }
        if bytes[i] == b'$' {
            if let Some(tag) = dollar_tag(sql, i) {
                dollar = Some(tag.clone());
                i += tag.len() + 2;
                continue;
            }
        }
        if bytes[i] == b';' {
            push_piece(sql, start, i + 1, &mut out);
            start = i + 1;
        }
        i += 1;
    }
    push_piece(sql, start, sql.len(), &mut out);
    out
}

pub fn first_keyword(sql: &str) -> String {
    let stripped = strip_comments(sql);
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

#[cfg(test)]
pub fn statement_at(sql: &str, cursor: usize) -> Option<Piece> {
    let pieces = split_sql(sql);
    if let Some(found) = pieces.iter().find(|piece| cursor >= piece.start && cursor <= piece.end) {
        return Some(found.clone());
    }
    pieces.into_iter().filter(|piece| piece.end <= cursor).next_back()
}

fn push_piece(sql: &str, start: usize, end: usize, out: &mut Vec<Piece>) {
    let raw = &sql[start..end];
    let lead = leading_noise(raw);
    let body = raw[lead..].trim_end();
    if !meaningful(body) {
        return;
    }
    let text_start = start + lead;
    out.push(Piece {
        text: body.to_owned(),
        start: text_start,
        end: text_start + body.len(),
    });
}

fn leading_noise(raw: &str) -> usize {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
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

fn meaningful(text: &str) -> bool {
    let stripped = strip_comments(text);
    stripped.chars().any(|ch| !ch.is_whitespace())
}

fn strip_comments(sql: &str) -> String {
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
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
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

fn dollar_tag(sql: &str, start: usize) -> Option<String> {
    let rest = &sql[start + 1..];
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
