//! 适配器边界扫描（ADR-0 / R22）。
//!
//! 扫描 `src/` 下除 `adapters/` 与测试代码（`tests.rs`、`*_tests.rs`、`#[cfg(test)]` 项）以外的生产源码，
//! 禁止出现：具体 `Engine::<变体>`、具体会话变体（`CatalogSession::X` 等）、引擎名字符串字面量与方言标志性 SQL 片段。
//!
//! 迁移期间 [`ALLOWLIST`] 按文件记录尚未迁移的现存命中数，理由是负责迁移它的任务领域。
//! 计数只减不增：命中数超过记录、或白名单外文件出现命中都会失败；命中减少时也会失败并提示把记录下调
//! （棘轮），清零的条目必须删除。最后一个迁移任务（A6）完成时白名单为空。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 具体引擎变体。
const ENGINE_VARIANTS: &[&str] = &[
    "Engine::Mysql",
    "Engine::Mariadb",
    "Engine::Postgres",
    "Engine::Sqlite",
    "Engine::Sqlserver",
    "Engine::Redis",
    "Engine::Mongodb",
];

/// 会话枚举：其后紧跟大写字母即视为具体会话变体。
const SESSION_ENUMS: &[&str] = &["CatalogSession::", "Attached::", "CancelTransport::", "SessionKind::"];

/// 引擎名字符串字面量（整个字面量，大小写不敏感）。
const ENGINE_NAMES: &[&str] = &[
    "mysql",
    "mariadb",
    "postgres",
    "postgresql",
    "sqlite",
    "sqlserver",
    "sql server",
    "mssql",
    "redis",
    "mongodb",
    "mongo",
];

/// 方言标志性 SQL 片段（大小写敏感）。
const DIALECT_FRAGMENTS: &[&str] = &[
    "`{",
    "}`",
    "'`'",
    "\"`\"",
    "PRAGMA",
    "pragma ",
    "pg_",
    "information_schema",
    "INFORMATION_SCHEMA",
    "sys.",
    "sqlite_master",
    "TOP ",
    "FETCH NEXT",
    "ILIKE",
    "AUTO_INCREMENT",
    "auto_increment",
    "IDENTITY",
    "sp_rename",
    "MyISAM",
    "InnoDB",
    "ALGORITHM",
    "SQL SECURITY",
    "\"algorithm\"",
    "\"definer\"",
    "WITHOUT ROWID",
];

const A1: &str = "A1 连接与会话";
const A2: &str = "A2 目录与关系图";
const A3: &str = "A3 DDL 与结构/管理";
const A4: &str = "A4 网格与查询";
const A5: &str = "A5 对比/转储/迁移/导出";
const A6: &str = "A6 UI 引擎元数据与收尾";

/// 负责迁移该文件的任务领域（按执行计划各任务的写入范围，先匹配者优先）。
fn domain_of(file: &str) -> &'static str {
    const RULES: &[(&str, &str)] = &[
        ("application/ddl.rs", A3),
        ("application/grid.rs", A4),
        ("query/cancel.rs", A1),
        ("query/exec.rs", A4),
        ("connections/", A1),
        ("application/", A1),
        ("redis/", A1),
        ("mongo/", A1),
        ("app/views/connections/", A1),
        ("app/shell/conn_modal.rs", A1),
        ("app/shell/conn_page.rs", A1),
        ("app/views/redis/", A1),
        ("app/views/mongo/", A1),
        ("catalog/", A2),
        ("diagram/", A2),
        ("app/views/diagram/", A2),
        ("app/shell/nav.rs", A2),
        ("ddl/", A3),
        ("app/views/structure/", A3),
        ("app/views/admin/", A3),
        ("grid/", A4),
        ("query/", A4),
        ("app/views/grid/", A4),
        ("app/views/query/", A4),
        ("ui/editor_proto.rs", A4),
        ("compare/", A5),
        ("dump/", A5),
        ("migrate/", A5),
        ("export/", A5),
        ("native_export.rs", A5),
        ("app/views/compare/", A5),
        ("app/views/export/", A5),
    ];
    RULES.iter().find(|(prefix, _)| file.starts_with(prefix)).map(|(_, domain)| *domain).unwrap_or(A6)
}

/// 尚未迁移的文件与现存命中数（只减不增）。理由为负责迁移的任务领域。
const ALLOWLIST: &[(&str, usize, &str)] = &[
    ("app/model.rs", 3, A6),
    ("app/shell/chrome.rs", 32, A6),
    ("app/shell/model.rs", 19, A6),
    ("app/shell/view.rs", 8, A6),
    ("i18n/mod.rs", 6, A6),
    ("ui/format_viewer.rs", 1, A6),
    ("ui/kit.rs", 27, A6),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `src/` 下全部 Rust 源码：(相对 src 的路径, 内容)。
fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    rust_files(&root, &mut paths);
    paths
        .into_iter()
        .map(|path| {
            let name = path.strip_prefix(&root).unwrap().to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
            (name, std::fs::read_to_string(&path).unwrap())
        })
        .collect()
}

/// 适配器层与测试文件不受约束。
fn exempt(file: &str) -> bool {
    let base = file.rsplit('/').next().unwrap_or(file);
    file.starts_with("adapters/") || base == "tests.rs" || base.ends_with("_tests.rs") || file.contains("/tests/")
}

fn is_ident(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// `i` 处若是字符串/字符/原始字符串字面量，返回其后一位；否则 None（含生命周期）。
fn literal_end(chars: &[char], i: usize) -> Option<usize> {
    let at = |k: usize| chars.get(k).copied();
    match chars[i] {
        '"' => {
            let mut k = i + 1;
            while k < chars.len() {
                match chars[k] {
                    '\\' => k += 2,
                    '"' => return Some(k + 1),
                    _ => k += 1,
                }
            }
            Some(chars.len())
        }
        'r' => {
            let prev = if i == 0 { None } else { at(i - 1) };
            let prefix_ok = match prev {
                None => true,
                Some('b') => i < 2 || !at(i - 2).is_some_and(is_ident),
                Some(ch) => !is_ident(ch),
            };
            if !prefix_ok {
                return None;
            }
            let mut k = i + 1;
            let mut hashes = 0;
            while at(k) == Some('#') {
                hashes += 1;
                k += 1;
            }
            if at(k) != Some('"') {
                return None;
            }
            k += 1;
            while k < chars.len() {
                if chars[k] == '"' && (1..=hashes).all(|n| at(k + n) == Some('#')) {
                    return Some(k + 1 + hashes);
                }
                k += 1;
            }
            Some(chars.len())
        }
        '\'' => {
            if at(i + 1) == Some('\\') {
                let mut k = i + 3;
                while k < chars.len() && chars[k] != '\'' {
                    k += 1;
                }
                Some((k + 1).min(chars.len()))
            } else if at(i + 2) == Some('\'') {
                Some(i + 3)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 去掉行注释与块注释（保留字面量与换行）。
fn strip_comments(text: &str) -> Vec<char> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
        } else if let Some(end) = literal_end(&chars, i) {
            out.extend_from_slice(&chars[i..end]);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 从 `start` 起跳过一个条目：到深度 0 的 `;`，或与首个 `{` 配对的 `}` 为止。
fn item_end(chars: &[char], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < chars.len() {
        if let Some(end) = literal_end(chars, i) {
            i = end;
            continue;
        }
        match chars[i] {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            ';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    chars.len()
}

/// 产品代码：去掉注释与 `#[cfg(test)]` 条目。
fn product_code(text: &str) -> String {
    const MARK: &str = "#[cfg(test)]";
    let mut chars = strip_comments(text);
    let mark: Vec<char> = MARK.chars().collect();
    let mut i = 0;
    while i + mark.len() <= chars.len() {
        if let Some(end) = literal_end(&chars, i) {
            i = end;
            continue;
        }
        if chars[i..i + mark.len()] == mark[..] {
            let end = item_end(&chars, i + mark.len());
            chars.drain(i..end);
            continue;
        }
        i += 1;
    }
    chars.into_iter().collect()
}

/// 一份产品代码中各禁止模式的命中数。
fn hits(code: &str) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    let mut add = |pattern: &str, count: usize| {
        if count > 0 {
            *found.entry(pattern.to_owned()).or_insert(0) += count;
        }
    };
    for pattern in ENGINE_VARIANTS.iter().chain(DIALECT_FRAGMENTS) {
        add(pattern, code.matches(pattern).count());
    }
    for prefix in SESSION_ENUMS {
        let count = code
            .match_indices(prefix)
            .filter(|(at, _)| code[at + prefix.len()..].chars().next().is_some_and(|ch| ch.is_ascii_uppercase()))
            .count();
        add(&format!("{prefix}<变体>"), count);
    }
    let lower = code.to_lowercase();
    for name in ENGINE_NAMES {
        let literal = format!("\"{name}\"");
        add(&literal, lower.matches(&literal).count());
    }
    found
}

/// 每个受约束文件的命中总数与明细（只列有命中的文件）。
fn scan(files: &[(String, String)]) -> BTreeMap<String, (usize, BTreeMap<String, usize>)> {
    files
        .iter()
        .filter(|(name, _)| !exempt(name))
        .filter_map(|(name, text)| {
            let detail = hits(&product_code(text));
            let total: usize = detail.values().sum();
            (total > 0).then(|| (name.clone(), (total, detail)))
        })
        .collect()
}

/// 与白名单比对，返回全部问题。
fn check(files: &[(String, String)], allowlist: &[(&str, usize, &str)]) -> Vec<String> {
    let found = scan(files);
    let mut problems = Vec::new();
    for (file, (total, detail)) in &found {
        match allowlist.iter().find(|(name, _, _)| name == file) {
            None => problems.push(format!("{file}: 新增适配器边界违规 {total} 处 {detail:?}；方言逻辑应写入 adapters/")),
            Some((_, allowed, _)) if total > allowed => {
                problems.push(format!("{file}: 违规由 {allowed} 增至 {total} 处 {detail:?}；白名单只减不增"))
            }
            Some((_, allowed, _)) if total < allowed => {
                problems.push(format!("{file}: 违规已由 {allowed} 减至 {total}，请把白名单下调为 {total}"))
            }
            Some(_) => {}
        }
    }
    for (file, _, reason) in allowlist {
        if !found.contains_key(*file) {
            problems.push(format!("{file}: 已无违规或文件不存在，请从白名单删除"));
        }
        if *reason != domain_of(file) {
            problems.push(format!("{file}: 白名单理由应为“{}”", domain_of(file)));
        }
    }
    problems
}

/// 失败时打印的当前白名单（便于迁移任务核对后更新）。
fn current_table(files: &[(String, String)]) -> String {
    scan(files)
        .iter()
        .map(|(file, (total, _))| format!("    (\"{file}\", {total}, {}),", const_name(domain_of(file))))
        .collect::<Vec<_>>()
        .join("\n")
}

fn const_name(domain: &str) -> &'static str {
    [(A1, "A1"), (A2, "A2"), (A3, "A3"), (A4, "A4"), (A5, "A5"), (A6, "A6")]
        .iter()
        .find(|(value, _)| *value == domain)
        .map(|(_, name)| *name)
        .unwrap()
}

#[test]
fn production_code_respects_the_adapter_boundary() {
    let files = sources();
    assert!(files.len() > 100, "扫描到了 src/ 的源码");
    assert!(files.iter().any(|(name, _)| name == "adapters/registry.rs"));
    let problems = check(&files, ALLOWLIST);
    assert!(
        problems.is_empty(),
        "适配器边界检查失败：\n{}\n\n当前命中（核对后可据此更新 ALLOWLIST，只减不增）：\n{}",
        problems.join("\n"),
        current_table(&files)
    );
    for (file, count, reason) in ALLOWLIST {
        assert!(*count > 0 && !reason.is_empty(), "{file}");
    }
    // 已完成迁移的领域不得再出现在白名单里。
    let done: &[&str] = &[A1, A2, A3, A4, A5];
    assert!(
        ALLOWLIST.iter().all(|(_, _, reason)| !done.contains(reason)),
        "{done:?} 已迁移完毕，白名单不应再有它们的条目"
    );
}

/// 自测：在现存源码上人为加入一处新违规，扫描必须失败；测试代码、注释与 adapters/ 不受约束。
#[test]
fn boundary_scan_rejects_injected_violations() {
    let files = sources();
    assert!(check(&files, ALLOWLIST).is_empty(), "基线应通过");

    let inject = |file: &str, extra: &str| -> Vec<(String, String)> {
        let mut changed = files.clone();
        match changed.iter_mut().find(|(name, _)| name == file) {
            Some((_, text)) => text.push_str(extra),
            None => changed.push((file.to_owned(), extra.to_owned())),
        }
        changed
    };
    let violations = [
        "\nfn injected(engine: Engine) -> bool { engine == Engine::Mysql }\n",
        "\nfn injected(kind: &str) -> bool { kind == \"PostgreSQL\" }\n",
        "\nconst INJECTED: &str = \"SELECT name FROM sqlite_master\";\n",
        "\nfn injected(session: &CatalogSession) -> bool { matches!(session, CatalogSession::Redis(_)) }\n",
        "\nfn injected(name: &str) -> String { format!(\"`{}`\", name) }\n",
        "\nfn injected(storage: &str) -> bool { storage.eq_ignore_ascii_case(\"MyISAM\") }\n",
        "\nfn injected(word: &str) -> bool { word.eq_ignore_ascii_case(\"definer\") }\n",
        "\nconst INJECTED: &str = \"CREATE ALGORITHM=MERGE VIEW v AS SELECT 1\";\n",
    ];
    // 一个白名单内文件（若有）与一个白名单外新文件都要被抓到。
    let mut targets = vec!["app/injected_view.rs".to_owned()];
    if let Some((file, _, _)) = ALLOWLIST.first() {
        targets.push((*file).to_owned());
    }
    for target in &targets {
        for violation in violations {
            let problems = check(&inject(target, violation), ALLOWLIST);
            assert!(problems.iter().any(|problem| problem.starts_with(target.as_str())), "{target} 注入 {violation} 未被拦截");
        }
    }

    let tolerated = [
        ("app/injected_view.rs", "\n// Engine::Mysql 与 \"mysql\" 只出现在注释里\n/* PRAGMA { */ fn ok() {}\n"),
        ("app/injected_view.rs", "\n#[cfg(test)]\nmod t {\n    fn f() { let _ = (Engine::Mysql, \"}\", '{', r#\"}\"#); }\n}\nfn ok() {}\n"),
        ("app/injected_view.rs", "\n#[cfg(test)]\nuse crate::connections::model::Engine::Sqlite;\n"),
        ("adapters/mysql/injected.rs", "\nfn f() { let _ = (Engine::Mysql, \"mysql\", \"PRAGMA\"); }\n"),
        ("app/injected_tests.rs", "\nfn f() { let _ = Engine::Postgres; }\n"),
        ("app/views/tests.rs", "\nfn f() { let _ = \"information_schema\"; }\n"),
    ];
    for (file, extra) in tolerated {
        let problems = check(&inject(file, extra), ALLOWLIST);
        assert!(problems.is_empty(), "{file} 不应报违规：{problems:?}");
    }

    // 白名单只减不增：计数下降或文件已清零也必须报出，提醒下调/删除条目。
    let shrink = [("app/injected_view.rs", 2, A6)];
    let extra = "\nfn f() -> bool { matches!(e, Engine::Redis) }\n";
    let problems = check(&inject("app/injected_view.rs", extra), &shrink);
    assert!(problems.iter().any(|problem| problem.contains("下调为 1")), "{problems:?}");
    let problems = check(&files, &shrink);
    assert!(problems.iter().any(|problem| problem.contains("从白名单删除")), "{problems:?}");
}

#[test]
fn product_code_strips_comments_and_test_items_only() {
    let text = "fn a() { let s = \"// not a comment {\"; }\n// gone\n#[cfg(test)]\nmod t { fn b() { let _ = '}'; } }\nfn c<'a>(x: &'a str) {}\n";
    let code = product_code(text);
    assert!(code.contains("\"// not a comment {\""));
    assert!(!code.contains("gone"));
    assert!(!code.contains("mod t"));
    assert!(code.contains("fn c<'a>(x: &'a str) {}"));
    assert_eq!(domain_of("connections/service.rs"), A1);
    assert_eq!(domain_of("application/ddl.rs"), A3);
    assert_eq!(domain_of("query/cancel.rs"), A1);
    assert_eq!(domain_of("query/split.rs"), A4);
    assert_eq!(domain_of("app/shell/view.rs"), A6);
}
