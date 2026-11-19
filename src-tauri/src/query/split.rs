//! 按引擎切分用户 SQL。只用于决定发送范围，不判断语句是否安全。
//! 行注释切换也放在这里：同样是按行切文本的纯变换，不依赖编辑器状态。

use std::ops::Range;

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
    /// 拒绝片段的起点。光标落在这里或之后时，不能退回前面已经闭合的语句。
    pub refusal_from: Option<usize>,
}

/// 中文输入法的全角分号。
const FULLWIDTH_SEMICOLON: &str = "\u{ff1b}";

pub fn split_sql(engine: &str, sql: &str) -> SplitReport {
    let lexis = crate::adapters::query::lexis_named(engine);
    let hash = lexis.hash_comments;
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
        // T-SQL 的 GO 是客户端批次分隔，不是发往服务器的语句，整行跳过。
        if lexis.transact && is_go_line(sql, begin) {
            let line_end = eol_at(sql, begin);
            i = skip_break(sql, line_end);
            continue;
        }
        if is_delimiter_command(sql, begin) {
            let line_end = eol_at(sql, begin);
            let arg = delimiter_argument(&sql[begin..line_end]);
            if !lexis.delimiter_command {
                if !noticed {
                    notices.push(format!(
                        "{} 不使用 DELIMITER 这条客户端指令，已跳过。其余语句仍按该引擎执行。",
                        crate::adapters::query::label_named(engine)
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
                    refusal_from: Some(begin),
                };
            };
            delimiter = arg;
            i = skip_break(sql, line_end);
            continue;
        }
        let scanned = scan_statement(sql, begin, &delimiter, hash, lexis.transact);
        if let Some(piece) = scanned.piece {
            pieces.push(piece);
        }
        if let Some(refusal) = scanned.refusal {
            return SplitReport {
                pieces,
                notices,
                refusal: Some(refusal),
                refusal_from: Some(begin),
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
        refusal_from: None,
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

/// 只读语句允许的开头关键字。其余开头（SET、PRAGMA、EXEC、RENAME、COMMENT 等）一律按可能写入处理。
const READ_ONLY_LEADS: &[&str] = &["SELECT", "SHOW", "EXPLAIN", "DESCRIBE", "DESC", "WITH"];

/// 出现在语句任意位置（含括号内的 CTE 体）即视为可能写入的代码词。
/// INTO 覆盖 SELECT ... INTO 新表/文件；UPDATE 同时让 FOR UPDATE 锁行查询走确认。
/// 其他写入语句的开头（COMMENT/VACUUM/COPY 等）不在 READ_ONLY_LEADS 里，已经按写入处理。
const WRITE_WORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "UPSERT", "INTO", "CREATE", "ALTER", "DROP", "TRUNCATE", "RENAME",
    "GRANT", "REVOKE", "EXEC", "EXECUTE", "CALL",
];

/// 保守的只读判定：整段 SQL 能切分且每一条语句都明显只读时才为 true。
/// 只读 = 以 SELECT/SHOW/EXPLAIN/DESCRIBE/DESC/WITH 开头，且字符串与注释以外的任何层级
/// 都没有写入类关键字（覆盖 WITH d AS (DELETE ...) SELECT、EXPLAIN ANALYZE DELETE、SELECT ... INTO）。
/// 切分被拒绝、没有语句或任何一条不确定时都返回 false：调用方应当把它当作写入。
/// 方言专有的副作用词（T-SQL 无分号相接的 KILL/DBCC/RESTORE 等）由适配器提供；
/// MySQL 家族的 `/*! ... */` 可执行注释按代码检查。
pub fn is_clearly_read_only(engine: &str, sql: &str) -> bool {
    let report = split_sql(engine, sql);
    if report.refusal.is_some() || report.pieces.is_empty() {
        return false;
    }
    let dialect = crate::adapters::query::side_effect_words_named(engine);
    let executable = crate::adapters::query::lexis_named(engine).executable_comments;
    report.pieces.iter().all(|piece| {
        let (words, _) = code_words(engine, &piece.text, false, executable);
        let Some((lead, _, _)) = words.first() else { return false };
        READ_ONLY_LEADS.contains(&lead.as_str())
            && !words.iter().any(|(word, _, _)| WRITE_WORDS.contains(&word.as_str()) || dialect.contains(&word.as_str()))
    })
}

/// 顶层代码词及其字节区间。字符串、注释、dollar-quote 和括号内的词不出现。
pub fn top_level_words(engine: &str, sql: &str) -> (Vec<(String, usize, usize)>, usize) {
    code_words(engine, sql, true, false)
}

/// 代码词及其字节区间：字符串、注释、dollar-quote 与方括号标识符里的词不出现；
/// top_only 时只收括号外的词。executable 时 `/*!`、`/*M!` 注释里的内容按代码扫描。
fn code_words(engine: &str, sql: &str, top_only: bool, executable: bool) -> (Vec<(String, usize, usize)>, usize) {
    let bytes = sql.as_bytes();
    let lexis = crate::adapters::query::lexis_named(engine);
    let hash = lexis.hash_comments;
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut in_executable = false;
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
        if in_executable && sql[i..].starts_with("*/") {
            in_executable = false;
            i += 2;
            continue;
        }
        if executable && !in_executable {
            let opener = if sql[i..].starts_with("/*!") {
                Some(3)
            } else if sql[i..].starts_with("/*M!") || sql[i..].starts_with("/*m!") {
                Some(4)
            } else {
                None
            };
            if let Some(len) = opener {
                i += len;
                // 可选的版本号：该版本及以上的服务器会执行内容。
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                in_executable = true;
                continue;
            }
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
        if crate::adapters::query::is_quote_byte(bytes[i]) {
            quote = Some(bytes[i]);
            i += 1;
            end_of_code = i;
            continue;
        }
        // T-SQL 的 [name] 标识符：]] 转义；其中的词不参与顶层关键字判断（[top] 不是 TOP）。
        if lexis.transact && bytes[i] == b'[' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b']' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += utf8_len(bytes[i]);
            }
            end_of_code = i.min(bytes.len());
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
        if (depth == 0 || !top_only) && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
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
    let report = split_sql(engine, sql);
    if let Some(found) = report.pieces.iter().find(|piece| cursor >= piece.start && cursor <= piece.end) {
        return Some(found.clone());
    }
    if report.refusal.is_some()
        && report.refusal_from.is_some_and(|from| cursor >= from) {
            return None;
        }
    if let Some(previous) = report.pieces.iter().rev().find(|piece| piece.end <= cursor) {
        return Some(previous.clone());
    }
    None
}

struct Scanned {
    piece: Option<Piece>,
    next: usize,
    refusal: Option<String>,
}

fn scan_statement(sql: &str, start: usize, delimiter: &str, hash: bool, tsql_blocks: bool) -> Scanned {
    let bytes = sql.as_bytes();
    let mut i = start;
    let mut quote: Option<u8> = None;
    let mut dollar: Option<String> = None;
    let mut block = false;
    // T-SQL 的方括号标识符 [name]，]] 是转义的 ]。
    let mut bracket = false;
    let mut paren = 0i32;
    let mut begin_depth = 0i32;
    let mut routine = false;
    let mut opened_block = false;
    // END TRY 之后等待 BEGIN CATCH：期间分号不结束语句。
    let mut catch_pending = false;
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
        // 方括号标识符态先于注释判定：[a--b]/[a/*b] 里的 -- 与 /* 是标识符内容，
        // 与 TS 镜像（split.ts）的检查顺序同序。
        if tsql_blocks && bracket {
            if bytes[i] == b']' {
                if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                    i += 2;
                    continue;
                }
                bracket = false;
                i += 1;
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
        if tsql_blocks && bytes[i] == b'[' {
            bracket = true;
            i += 1;
            continue;
        }
        if crate::adapters::query::is_quote_byte(bytes[i]) {
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
            // T-SQL 的 BEGIN...END 不只出现在例程体里：顶层批处理块同样按深度计数，
            // 块内的分号不结束语句。BEGIN TRAN/TRANSACTION/DISTRIBUTED TRAN 是事务，不是块。
            // CASE 表达式同样以 END 收尾，成对计入深度，避免块内的 CASE END 提前扣减。
            let block_keyword = routine
                || (tsql_blocks && (upper == "CASE" || !begins_transaction(sql, word.1, hash)));
            // GO 独占一行时是批次分隔：当前语句在这里结束，GO 本身不进语句文本。
            if tsql_blocks && upper == "GO" && begin_depth == 0 && is_go_line(sql, i) {
                closed = true;
                close_at = i;
                i = word.1;
                break;
            }
            if upper == "CATCH" {
                catch_pending = false;
            }
            if (upper == "BEGIN" && block_keyword) || (upper == "CASE" && tsql_blocks) {
                opened_block = true;
                begin_depth += 1;
            } else if upper == "END" && block_keyword && begin_depth > 0 {
                let next = peek_word(sql, word.1, hash);
                if next.as_deref().map(|item| !is_routine_tail(item)).unwrap_or(true) {
                    begin_depth -= 1;
                    if tsql_blocks && matches!(next.as_deref(), Some("TRY") | Some("CATCH")) {
                        // END TRY / END CATCH 是复合收尾，语句边界在第二个词之后。
                        if next.as_deref() == Some("TRY") {
                            // TRY 块结束但整个 TRY/CATCH 还没完：等待 BEGIN CATCH，期间分号不切分。
                            catch_pending = true;
                        } else if begin_depth == 0 && opened_block {
                            let catch_end = end_of_peeked_word(sql, word.1, hash);
                            closed = true;
                            close_at = catch_end;
                            i = close_at;
                            break;
                        }
                    } else if begin_depth == 0 && opened_block && tsql_blocks && next.as_deref() != Some("ELSE") {
                        // 块回到顶层时语句自闭合：END 后面不是 ELSE/TRY/CATCH 就在这里结束。
                        // 例程体（CREATE PROCEDURE 等）保持原有行为，仍按分号或文末收束。
                        closed = true;
                        close_at = word.1;
                        i = close_at;
                        break;
                    }
                }
            }
            i = word.1;
            continue;
        }
        if !catch_pending && paren == 0 && begin_depth == 0 && !delimiter.is_empty() && sql[i..].starts_with(delimiter) {
            closed = true;
            close_at = i + delimiter.len();
            i = close_at;
            break;
        }
        // 中文输入法下常打出全角分号“；”：在字符串、注释与标识符之外按语句结束符处理，
        // 不让它跟着语句发给数据库（也不让自动追加的 LIMIT 接在它后面）。
        if !catch_pending && paren == 0 && begin_depth == 0 && delimiter == ";" && sql[i..].starts_with(FULLWIDTH_SEMICOLON) {
            closed = true;
            close_at = i + FULLWIDTH_SEMICOLON.len();
            i = close_at;
            break;
        }
        i += utf8_len(bytes[i]);
    }
    if quote.is_some() {
        return refuse("引号没有闭合，没有发送。");
    }
    if bracket {
        return refuse("方括号标识符没有闭合，没有发送。");
    }
    if block {
        return refuse("块注释没有闭合，没有发送。");
    }
    if dollar.is_some() {
        return refuse("dollar-quote 没有闭合，没有发送。");
    }
    if begin_depth > 0 {
        return refuse(if tsql_blocks { "BEGIN 块未闭合，没有发送。" } else { "例程体未闭合，没有发送。" });
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
    let trimmed = raw[lead..].trim_end();
    // 全角分号只是结束符，不属于语句本身。
    let body = trimmed.strip_suffix(FULLWIDTH_SEMICOLON).map(str::trim_end).unwrap_or(trimmed);
    // 只剩分号（块自闭合后残留的结束符）不算一条语句。
    if !meaningful(&body.replace(';', " "), hash) {
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
        while let Some(word) = words.get(index).map(String::as_str) {
            if matches!(word, "PROCEDURE" | "FUNCTION" | "TRIGGER") {
                break;
            }
            if is_not_routine(word) {
                return Look::No;
            }
            index += 1;
        }
    }
    match words.get(index).map(String::as_str) {
        None => Look::Maybe,
        Some("PROCEDURE" | "FUNCTION" | "TRIGGER") => Look::Yes,
        Some(_) => Look::No,
    }
}

fn is_not_routine(word: &str) -> bool {
    matches!(
        word,
        "TABLE"
            | "VIEW"
            | "INDEX"
            | "UNIQUE"
            | "FULLTEXT"
            | "SPATIAL"
            | "DATABASE"
            | "SCHEMA"
            | "USER"
            | "ROLE"
            | "EVENT"
            | "SERVER"
            | "TABLESPACE"
            | "LOGFILE"
            | "SEQUENCE"
            | "TYPE"
            | "EXTENSION"
            | "TEMPORARY"
            | "TEMP"
            | "UNLOGGED"
            | "MATERIALIZED"
            | "RECURSIVE"
            | "POLICY"
            | "RULE"
            | "DOMAIN"
            | "CAST"
            | "OPERATOR"
            | "AGGREGATE"
            | "COLLATION"
            | "CONVERSION"
            | "LANGUAGE"
    )
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

/// T-SQL 的 BEGIN 后面跟 TRAN/TRANSACTION/DISTRIBUTED 时是事务语句，不进入块计数。
fn begins_transaction(sql: &str, index: usize, hash: bool) -> bool {
    matches!(peek_word(sql, index, hash).as_deref(), Some("TRAN") | Some("TRANSACTION") | Some("DISTRIBUTED"))
}

/// 复合收尾（END TRY / END CATCH）第二个词的结束位置。
fn end_of_peeked_word(sql: &str, from: usize, hash: bool) -> usize {
    let at = skip_noise(sql, from, hash);
    read_word(sql, at).1
}

/// GO 批次分隔的判定：GO 独占一行（行内剩余只允许空白或纯数字的批次计数），行中出现的 GO 不算。
fn is_go_line(sql: &str, word_start: usize) -> bool {
    let bytes = sql.as_bytes();
    if !read_word(sql, word_start).0.eq_ignore_ascii_case("GO") {
        return false;
    }
    let mut back = word_start;
    while back > 0 && (bytes[back - 1] == b' ' || bytes[back - 1] == b'\t' || bytes[back - 1] == b'\r') {
        back -= 1;
    }
    if back > 0 && bytes[back - 1] != b'\n' {
        return false;
    }
    let line_end = eol_at(sql, word_start);
    // GO 后面允许：重复次数（GO 5）、行尾注释（GO -- 第二批）与一个多写的分号（GO;）。
    let rest = sql[word_start + 2..line_end].trim();
    let rest = rest.split_once("--").map(|(code, _)| code).unwrap_or(rest).trim();
    let rest = rest.strip_suffix(';').unwrap_or(rest).trim();
    rest.is_empty() || rest.bytes().all(|byte| byte.is_ascii_digit())
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
        if crate::adapters::query::is_quote_byte(bytes[i]) {
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
    // 第一个字符可能是多字节字符（如中文正文），按字符长度跳过，不切在字符中间。
    let mut end = index + sql[index..].chars().next().map_or(1, char::len_utf8);
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

/// 一次行注释切换带来的编辑：整段受影响的行只替换一次，撤销栈里只有一条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentEdit {
    /// 受影响的连续行（字节范围），不含最后一行的换行符；换行本身留在原处。
    pub range: Range<usize>,
    /// 这段范围要换成的新文本。
    pub text: String,
    /// 替换之后应当恢复的选区（新文档的字节坐标）。
    pub selection: Range<usize>,
    /// 这次是加上一层注释（true）还是去掉一层（false）。
    pub commented: bool,
}

/// SQL 通用行注释标记：标记本身加一个空格。
const COMMENT: &str = "-- ";
/// 判断“已经注释”时只看标记，不要求后面有空格。
const MARK: &str = "--";

fn line_start(sql: &str, at: usize) -> usize {
    sql[..at].rfind('\n').map(|pos| pos + 1).unwrap_or(0)
}

fn line_end(sql: &str, from: usize) -> usize {
    let from = floor_boundary(sql, from);
    sql[from..].find('\n').map(|at| from + at).unwrap_or(sql.len())
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// 拆出一行的前导空白与正文；CRLF 的行尾回车不算正文。
fn body_of(line: &str) -> (&str, &str) {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, body) = line.split_at(indent);
    (indent, body.trim_end_matches('\r'))
}

/// 去掉一层注释：标记及其后的一个可选空格。调用方保证这一行确实是注释。
fn uncomment(body: &str) -> &str {
    let rest = &body[MARK.len()..];
    rest.strip_prefix(' ').unwrap_or(rest)
}

/// 切换一段 SQL 的行注释，返回一次范围替换所需的数据。
///
/// - 没有选区时作用于光标所在行；有选区时作用到选区触及的全部行
///   （选区恰好结束在行首时不算下一行）。
/// - 只识别前导空白之后的注释标记；空行（含只有空白的行）保持原样。
/// - 所有非空行都已注释时去掉一层注释，否则给所有非空行加一层注释；
///   混合选区因此第一次统一注释、第二次统一取消。
/// - 保留缩进、CRLF 与 UTF-8；整段文本为空、或没有任何可改动的非空行时不动作。
pub fn toggle_line_comment(sql: &str, selection: Range<usize>) -> Option<CommentEdit> {
    if sql.is_empty() {
        return None;
    }
    let len = sql.len();
    let mut start = floor_boundary(sql, selection.start);
    let mut end = ceil_boundary(sql, selection.end);
    if end < start {
        std::mem::swap(&mut start, &mut end);
    }
    start = start.min(len);
    end = end.min(len);
    let region_start = line_start(sql, start);
    // 选区末字符所在的行：末字符是换行时它属于上一行，不会把下一行算进来。
    // 末字符可能落在多字节字符中间，按字符边界往回取。
    let last = if end > start { floor_boundary(sql, end - 1) } else { start };
    let region_end = line_end(sql, last.max(region_start));
    let region = &sql[region_start..region_end];

    let mut any_body = false;
    let mut all_commented = true;
    for line in region.split('\n') {
        let (_, body) = body_of(line);
        if body.is_empty() {
            continue;
        }
        any_body = true;
        if !body.starts_with(MARK) {
            all_commented = false;
        }
    }
    if !any_body {
        return None;
    }
    let commented = !all_commented;

    let mut first_delta: isize = 0;
    let mut rebuilt = String::with_capacity(region.len() + 8);
    for (index, line) in region.split('\n').enumerate() {
        if index > 0 {
            rebuilt.push('\n');
        }
        let (indent, body) = body_of(line);
        let cr = &line[indent.len() + body.len()..];
        let rewritten = if body.is_empty() {
            line.to_owned()
        } else if commented {
            format!("{indent}{COMMENT}{body}{cr}")
        } else {
            format!("{indent}{}{cr}", uncomment(body))
        };
        // 光标在第一行时按这一行的长度变化平移，仍停在同一个字符上。
        if index == 0 {
            first_delta = rewritten.len() as isize - line.len() as isize;
        }
        rebuilt.push_str(&rewritten);
    }

    let selection = if end > start {
        region_start..region_start + rebuilt.len()
    } else {
        let at = (start as isize + first_delta).clamp(region_start as isize, (region_start + rebuilt.len()) as isize) as usize;
        at..at
    };
    Some(CommentEdit { range: region_start..region_end, text: rebuilt, selection, commented })
}

#[cfg(test)]
mod comment_tests {
    use super::*;

    /// 把一次编辑作用回文本，得到切换后的完整 SQL。
    fn apply(sql: &str, selection: Range<usize>) -> (String, Range<usize>, bool) {
        let edit = toggle_line_comment(sql, selection).expect("应当发生的行注释切换");
        let mut next = String::with_capacity(sql.len() + edit.text.len());
        next.push_str(&sql[..edit.range.start]);
        next.push_str(&edit.text);
        next.push_str(&sql[edit.range.end..]);
        (next, edit.selection, edit.commented)
    }

    #[test]
    fn a_caret_without_a_selection_comments_and_restores_its_own_line() {
        let sql = "SELECT 1;";
        let (commented, selection, added) = apply(sql, 0..0);
        assert_eq!(commented, "-- SELECT 1;");
        assert!(added, "第一次是加注释");
        assert_eq!(selection, 3..3, "光标跟着原字符移动，不落在标记中间");
        let (restored, _, added) = apply(&commented, selection);
        assert_eq!(restored, sql, "第二次切换回到原 SQL");
        assert!(!added, "第二次是去掉注释");
        // 光标在行尾时同样只动这一行。
        let (commented, selection, _) = apply(sql, sql.len()..sql.len());
        assert_eq!(commented, "-- SELECT 1;");
        assert_eq!(selection, sql.len() + 3..sql.len() + 3);
    }

    #[test]
    fn a_multi_line_selection_comments_every_non_empty_line_and_leaves_blank_lines_alone() {
        let sql = "SELECT 1;\n\nSELECT 2;";
        let (commented, selection, added) = apply(sql, 0..sql.len());
        assert_eq!(commented, "-- SELECT 1;\n\n-- SELECT 2;");
        assert!(added);
        assert_eq!(selection, 0..commented.len(), "多行切换后整段仍在选区里");
        let (restored, _, added) = apply(&commented, 0..commented.len());
        assert_eq!(restored, sql, "只去掉这一层标记，空行与文本不变");
        assert!(!added);
    }

    #[test]
    fn a_selection_ending_at_a_line_start_does_not_touch_the_next_line() {
        let sql = "SELECT 1;\nSELECT 2;";
        // 选区包含第一行行尾的换行符：仍然只算第一行。
        let (commented, _, _) = apply(sql, 0.."SELECT 1;\n".len());
        assert_eq!(commented, "-- SELECT 1;\nSELECT 2;");
        // 从第二行行首开始：只算这一行。
        let (commented, _, _) = apply(sql, 10..sql.len());
        assert_eq!(commented, "SELECT 1;\n-- SELECT 2;");
    }

    #[test]
    fn indentation_is_preserved_on_both_directions() {
        let sql = "    SELECT 1;\n  SELECT 2;";
        let (commented, _, _) = apply(sql, 0..sql.len());
        assert_eq!(commented, "    -- SELECT 1;\n  -- SELECT 2;");
        let (restored, _, _) = apply(&commented, 0..commented.len());
        assert_eq!(restored, sql);
        // 只去掉一个可选空格：标记后有两个空格时留一个。
        let spaced = "  --  SELECT 1;";
        let (stripped, _, _) = apply(spaced, 0..spaced.len());
        assert_eq!(stripped, "   SELECT 1;");
    }

    #[test]
    fn a_mixed_selection_comments_everything_first_and_uncomments_on_the_second_pass() {
        let sql = "SELECT 1;\n-- SELECT 2;\n  --SELECT 3;";
        let (commented, _, added) = apply(sql, 0..sql.len());
        assert!(added, "混合选区第一次统一注释");
        assert_eq!(commented, "-- SELECT 1;\n-- -- SELECT 2;\n  -- --SELECT 3;");
        let (restored, _, added) = apply(&commented, 0..commented.len());
        assert!(!added, "第二次统一取消一层");
        assert_eq!(restored, sql);
    }

    #[test]
    fn comments_without_a_space_and_with_extra_spaces_still_count() {
        let sql = "--SELECT 1;";
        let (stripped, _, added) = apply(sql, 0..0);
        assert!(!added);
        assert_eq!(stripped, "SELECT 1;");
        let sql = "---- SELECT 1;";
        let (stripped, _, _) = apply(sql, 0..sql.len());
        assert_eq!(stripped, "-- SELECT 1;", "只去掉一层");
    }

    #[test]
    fn utf8_and_crlf_text_keeps_its_bytes_and_line_endings() {
        let sql = "SELECT '中文' AS 名称;\r\nSELECT 2;";
        let (commented, selection, _) = apply(sql, 0..sql.len());
        assert_eq!(commented, "-- SELECT '中文' AS 名称;\r\n-- SELECT 2;");
        assert!(commented.contains("\r\n"), "CRLF 不被改成 LF");
        assert_eq!(selection, 0..commented.len());
        let (restored, _, _) = apply(&commented, 0..commented.len());
        assert_eq!(restored, sql, "还原后 UTF-8 与 CRLF 都和原文一致");
        // 光标落在中文里：切换不切在字符中间。
        let at = sql.find('名').expect("中文位置");
        let (commented, selection, _) = apply(sql, at..at);
        assert_eq!(commented, "-- SELECT '中文' AS 名称;\r\nSELECT 2;");
        assert!(commented.is_char_boundary(selection.start), "光标仍在字符边界上");
        assert_eq!(selection.start, at + 3);
        // 选区边界落在多字节字符中间时，按最近的字符边界处理。
        let (commented, _, _) = apply(sql, at + 1..at + 2);
        assert_eq!(commented, "-- SELECT '中文' AS 名称;\r\nSELECT 2;");
    }

    #[test]
    fn blank_only_and_empty_documents_do_not_move() {
        assert_eq!(toggle_line_comment("", 0..0), None);
        assert_eq!(toggle_line_comment("\n\n", 0..2), None);
        assert_eq!(toggle_line_comment("   \n \t", 0..7), None);
        // 空行夹在选区里不算“已全部注释”，加注释时也保持原样。
        let sql = "SELECT 1;\n\n\nSELECT 2;";
        let (commented, _, _) = apply(sql, 0..sql.len());
        assert_eq!(commented, "-- SELECT 1;\n\n\n-- SELECT 2;");
    }

    #[test]
    fn the_replaced_range_covers_exactly_the_touched_lines() {
        let sql = "SELECT 1;\nSELECT 2;\nSELECT 3;";
        let edit = toggle_line_comment(sql, 12..12).expect("第二行内的光标");
        assert_eq!(edit.range, 10..19, "只覆盖光标所在行，不含换行");
        assert_eq!(edit.text, "-- SELECT 2;");
        let edit = toggle_line_comment(sql, 0..sql.len()).expect("全选");
        assert_eq!(edit.range, 0..sql.len(), "整段连续行一次替换，没有行尾换行时覆盖到末字符");
    }
}
