use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::Row;

use super::atomic::{stage, write_atomic_thread};
use super::document::{shell_from_legacy, APP_STATE_VERSION};
use super::legacy::{encode_utf16_le, Platform};
use super::{read_connection_export, read_connections, read_env_tags, read_history, read_secret, StateStore};
use crate::connections::model::{password_account, ssh_password_account};
use crate::connections::secrets::{KeyringStore, MemoryStore, SecretStore, KEYRING_SERVICE};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rillsql-state-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn legacy_json() -> (Value, Value) {
    let session = json!({
        "tabs": [
            {
                "id": "tab-订单",
                "title": "订单查询",
                "kind": "query",
                "draft": "SELECT '林夏😀' AS 名称;",
                "connectionId": "keep-id",
                "connectionName": "客户库",
                "database": "订单",
                "engine": "postgres",
                "connectionStatus": "connected",
                "targetText": "客户库 / 订单",
                "targetNotice": null,
                "path": null,
                "lastExecutedSql": null,
                "titleKey": {"key": "shell.tabs.query", "params": {"name": "订单"}}
            },
            {
                "id": "tab-table",
                "title": "customers",
                "kind": "table",
                "draft": "",
                "connectionId": "keep-id",
                "connectionName": "客户库",
                "database": "订单",
                "engine": "postgres",
                "connectionStatus": "disconnected",
                "targetText": "customers",
                "targetNotice": null,
                "path": null,
                "tablePane": "structure",
                "structureSection": "indexes",
                "changes": [{"column": "name", "value": "uncommitted-secret"}]
            }
        ],
        "activeId": "tab-订单"
    });
    let shell = json!({
        "theme": "dark",
        "density": "relaxed",
        "editorFont": 16,
        "editorTheme": "warm",
        "locale": "zh-CN",
        "pageSize": 50,
        "previewKB": 4,
        "objectPage": 80,
        "nullCopy": "empty",
        "queryTimeoutSec": 30,
        "autoCheckUpdates": false,
        "password": "should-not-persist",
        "sshPassphrase": "phrase-should-not-persist"
    });
    (session, shell)
}

fn fingerprint(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    walk_files(root, &mut files);
    files.sort();
    files.into_iter().map(|path| {
        let bytes = fs::read(&path).unwrap_or_default();
        let name = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        (name, bytes)
    }).collect()
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_files(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn write_windows(root: &Path, session: &str, shell: &str) {
    let dir = root.join("EBWebView").join("Default").join("Local Storage").join("leveldb");
    fs::create_dir_all(&dir).unwrap();
    let mut bytes = b"\x00leveldb\x01".to_vec();
    bytes.extend(b"_https://tauri.localhost\x00\x01rillsql.session\x00");
    bytes.extend(encode_utf16_le(session));
    bytes.extend(b"\xff\x00rillsql.shell");
    bytes.extend(encode_utf16_le(shell));
    fs::write(dir.join("000003.log"), bytes).unwrap();
}

async fn write_webkit(path: &Path, session: &[u8], shell: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB NOT NULL ON CONFLICT FAIL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO ItemTable (key, value) VALUES (?1, ?2)").bind("rillsql.session").bind(session).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO ItemTable (key, value) VALUES (?1, ?2)").bind("rillsql.shell").bind(shell).execute(&pool).await.unwrap();
    pool.close().await;
}

fn assert_imported(store: &StateStore) {
    let state = store.load().unwrap();
    assert_eq!(state.version, APP_STATE_VERSION);
    assert_eq!(state.shell.theme, "dark");
    assert_eq!(state.shell.density, "relaxed");
    assert_eq!(state.shell.editor_font, 16);
    assert_eq!(state.shell.editor_theme, "warm");
    assert_eq!(state.shell.locale, "zh-CN");
    assert_eq!(state.shell.page_size, 50);
    assert_eq!(state.shell.preview_kb, 4);
    assert_eq!(state.shell.object_page, 80);
    assert_eq!(state.shell.null_copy, "empty");
    assert_eq!(state.shell.query_timeout_sec, 30);
    assert!(!state.shell.auto_check_updates);
    assert_eq!(state.session.active_id.as_deref(), Some("tab-订单"));
    assert_eq!(state.session.tabs.len(), 2);
    assert_eq!(state.session.tabs[0].draft, "SELECT '林夏😀' AS 名称;");
    assert_eq!(state.session.tabs[0].title, "订单查询");
    assert_eq!(state.session.tabs[0].title_key.as_ref().map(|item| item.key.as_str()), Some("shell.tabs.query"));
    assert_eq!(state.session.tabs[1].kind, "table");
    assert_eq!(state.session.tabs[1].table_pane.as_deref(), Some("structure"));
    assert_eq!(state.session.tabs[1].structure_section.as_deref(), Some("indexes"));
    let text = fs::read_to_string(store.path()).unwrap();
    assert!(text.contains("林夏😀"), "{text}");
    assert!(!text.contains("should-not-persist"), "{text}");
    assert!(!text.contains("phrase-should-not-persist"), "{text}");
    assert!(!text.contains("uncommitted-secret"), "{text}");
    assert!(!text.contains("\"password\""), "{text}");
    assert!(!text.contains("sshPassphrase"), "{text}");
    let parent = store.path().parent().unwrap();
    let leftovers: Vec<_> = fs::read_dir(parent).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).filter(|name| name.contains(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn three_platform_fixtures_import_without_touching_sources() {
    let (session, shell) = legacy_json();
    let session_text = session.to_string();
    let shell_text = shell.to_string();
    let cases = [
        (Platform::Windows, "windows"),
        (Platform::Macos, "macos"),
        (Platform::Linux, "linux"),
    ];
    for (platform, name) in cases {
        let legacy = temp_dir(name);
        let data = temp_dir(&format!("{name}-data"));
        match platform {
            Platform::Windows => write_windows(&legacy, &session_text, &shell_text),
            Platform::Macos => {
                let path = legacy.join("WebKit").join("app.rillsql.desktop").join("WebsiteData").join("LocalStorage").join("https_tauri.localhost_0.localstorage");
                write_webkit(&path, &encode_utf16_le(&session_text), &encode_utf16_le(&shell_text)).await;
            }
            Platform::Linux => {
                let path = legacy.join("localstorage").join("localstorage.sqlite3");
                write_webkit(&path, session_text.as_bytes(), shell_text.as_bytes()).await;
            }
        }
        let before = fingerprint(&legacy);
        let store = StateStore::new(&data);
        let report = store.import_legacy(&legacy, platform).await;
        assert!(report.imported, "{platform:?} {}", report.warning.unwrap_or_default());
        assert!(report.wrote_state);
        assert_imported(&store);
        assert_eq!(fingerprint(&legacy), before, "{platform:?} 旧文件被改写");
        let _ = fs::remove_dir_all(&legacy);
        let _ = fs::remove_dir_all(&data);
    }
}

#[tokio::test]
async fn malformed_legacy_and_existing_state_are_left_in_place() {
    let (mut session, shell) = legacy_json();
    session["tabs"][0]["draft"] = json!(1);
    let legacy = temp_dir("bad");
    write_windows(&legacy, &session.to_string(), &shell.to_string());
    let before = fingerprint(&legacy);
    let data = temp_dir("bad-data");
    let store = StateStore::new(&data);
    let report = store.import_legacy(&legacy, Platform::Windows).await;
    assert!(!report.imported);
    assert!(!report.wrote_state);
    assert!(report.warning.as_deref().unwrap_or("").contains("没有写入"));
    assert!(!store.path().exists());
    assert_eq!(fingerprint(&legacy), before);

    let existing = super::document::AppState::new(super::document::ShellSettings::default(), super::document::SessionState::default());
    store.save(&existing).await.unwrap();
    let snapshot = fs::read(store.path()).unwrap();
    let unknown = data.join("unknown.json");
    fs::write(&unknown, b"{\"version\":9,\"nope\":true}\n").unwrap();
    let unknown_bytes = fs::read(&unknown).unwrap();
    assert!(super::parse_stored(&unknown_bytes).is_err());
    assert_eq!(fs::read(&unknown).unwrap(), unknown_bytes);

    let again = store.import_legacy(&legacy, Platform::Windows).await;
    assert!(!again.imported);
    assert!(!again.wrote_state);
    assert_eq!(fs::read(store.path()).unwrap(), snapshot);
    assert_eq!(fingerprint(&legacy), before);
    let _ = fs::remove_dir_all(&legacy);
    let _ = fs::remove_dir_all(&data);
}

#[tokio::test]
async fn atomic_write_uses_another_thread_and_abort_keeps_the_old_file() {
    let dir = temp_dir("atomic");
    let dest = dir.join("app-state.json");
    fs::write(&dest, b"OLD").unwrap();
    let staged = stage(&dest, b"NEW").unwrap();
    let err = staged.abort().unwrap_err();
    assert!(err.contains("未改动"), "{err}");
    assert_eq!(fs::read(&dest).unwrap(), b"OLD");
    let names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|name| !name.contains(".tmp")), "{names:?}");

    let caller = std::thread::current().id();
    let writer = write_atomic_thread(dest.clone(), b"NEW".to_vec()).await.unwrap();
    assert_ne!(caller, writer);
    assert_eq!(fs::read(&dest).unwrap(), b"NEW");
    let names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|name| !name.contains(".tmp")), "{names:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn comfortable_density_normalizes_and_unknown_shell_values_do_not_stick() {
    let shell = shell_from_legacy(&json!({"density": "comfortable", "editorTheme": "nope", "locale": "fr"})).unwrap();
    assert_eq!(shell.density, "standard");
    assert_eq!(shell.editor_theme, "auto");
    assert_eq!(shell.locale, "zh-CN");
}

#[tokio::test]
async fn old_sqlite_keyring_and_export_fixture_are_readable() {
    let dir = temp_dir("compat");
    let external = dir.join("订单.sqlite");
    fs::write(&external, b"external-db").unwrap();
    let connections = dir.join("connections.sqlite");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&connections).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE connections (
            id TEXT PRIMARY KEY, name TEXT NOT NULL, engine TEXT NOT NULL, env TEXT NOT NULL,
            host TEXT, port INTEGER, database_name TEXT, username TEXT, file_path TEXT,
            tls_mode TEXT, tls_server_name TEXT, ca_mode TEXT, ca_file TEXT, client_cert TEXT, client_key TEXT,
            ssh_host TEXT, ssh_port INTEGER, ssh_user TEXT, ssh_auth TEXT, ssh_key_path TEXT,
            password_saved INTEGER NOT NULL, ssh_password_saved INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO connections (
            id, name, engine, env, host, port, database_name, username, file_path,
            tls_mode, tls_server_name, ca_mode, ca_file, client_cert, client_key,
            ssh_host, ssh_port, ssh_user, ssh_auth, ssh_key_path, password_saved, ssh_password_saved
        ) VALUES (
            'keep-id', '客户😀', 'sqlite', 'local', NULL, NULL, NULL, NULL, ?1,
            NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 1, 1
        )",
    )
    .bind(external.to_string_lossy().to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("CREATE TABLE env_tags (id TEXT PRIMARY KEY, label TEXT NOT NULL, tone TEXT NOT NULL, position INTEGER NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO env_tags (id, label, tone, position) VALUES ('stage', '预发', 'prod', 0)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO app_meta (key, value) VALUES ('env_tags_seeded', '1')").execute(&pool).await.unwrap();
    pool.close().await;

    let listed = read_connections(&connections).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "keep-id");
    assert_eq!(listed[0].name, "客户😀");
    assert_eq!(listed[0].file_path.as_deref(), Some(external.to_string_lossy().as_ref()));
    assert!(listed[0].password_saved);
    let tags = read_env_tags(&connections).await.unwrap();
    assert!(tags.iter().any(|tag| tag.id == "stage" && tag.label == "预发"), "{tags:?}");
    assert_eq!(fs::read(&external).unwrap(), b"external-db");

    let history_path = dir.join("query-history.sqlite");
    let history = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&history_path).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE query_history (
            id TEXT PRIMARY KEY, sql TEXT NOT NULL, connection_id TEXT NOT NULL, connection_name TEXT NOT NULL,
            database_name TEXT NOT NULL, engine TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
            status TEXT NOT NULL, elapsed_ms INTEGER, summary TEXT NOT NULL
        )",
    )
    .execute(&history)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO query_history (
            id, sql, connection_id, connection_name, database_name, engine, started_at, finished_at, status, elapsed_ms, summary
        ) VALUES ('h1', ?1, 'keep-id', '客户库', '订单', 'postgres', '2020-01-01T00:00:00Z', NULL, 'ok', 3, '1 行')",
    )
    .bind("SELECT '林夏😀' AS 名称;")
    .execute(&history)
    .await
    .unwrap();
    history.close().await;
    let entries = read_history(&history_path).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "h1");
    assert_eq!(entries[0].sql, "SELECT '林夏😀' AS 名称;");
    assert_eq!(entries[0].connection_id, "keep-id");

    let secrets = Arc::new(MemoryStore::default());
    secrets.put(&password_account("keep-id"), "db-secret-不会进文件").unwrap();
    secrets.put(&ssh_password_account("keep-id"), "ssh-secret").unwrap();
    assert_eq!(KEYRING_SERVICE, "app.rillsql.desktop");
    assert_eq!(KeyringStore::new().service_name(), KEYRING_SERVICE);
    let _live_keyring = super::compat::keyring_store();
    assert_eq!(super::keyring_service(), KEYRING_SERVICE);
    assert_eq!(read_secret(secrets.as_ref(), &super::password_account_for("keep-id")).unwrap().as_deref(), Some("db-secret-不会进文件"));
    assert_eq!(read_secret(secrets.as_ref(), &super::ssh_password_account_for("keep-id")).unwrap().as_deref(), Some("ssh-secret"));
    let blob = fs::read(&connections).unwrap();
    let text = String::from_utf8_lossy(&blob);
    assert!(!text.contains("db-secret-不会进文件"));
    assert!(!text.contains("ssh-secret"));

    let export = include_bytes!("fixtures/connections-export.json");
    let parsed = read_connection_export(export, None).unwrap();
    assert_eq!(parsed.connections.len(), 1);
    assert_eq!(parsed.connections[0].id, "lite-legacy");
    assert_eq!(parsed.connections[0].name, "本地笔记😀");
    assert_eq!(parsed.connections[0].file_path.as_deref(), Some("C:/data/笔记.sqlite"));
    assert!(!parsed.connections[0].password_saved);
    assert!(!parsed.connections[0].ssh_password_saved);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn history_row_count_matches_inserted_legacy_row() {
    let dir = temp_dir("history-count");
    let path = dir.join("query-history.sqlite");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path).create_if_missing(true))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE query_history (
            id TEXT PRIMARY KEY, sql TEXT NOT NULL, connection_id TEXT NOT NULL, connection_name TEXT NOT NULL,
            database_name TEXT NOT NULL, engine TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
            status TEXT NOT NULL, elapsed_ms INTEGER, summary TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO query_history (
            id, sql, connection_id, connection_name, database_name, engine, started_at, finished_at, status, elapsed_ms, summary
        ) VALUES ('only', 'SELECT 1', 'c', 'n', 'd', 'sqlite', '2020-01-02T00:00:00Z', '2020-01-02T00:00:01Z', 'ok', 1, 'ok')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM query_history").fetch_one(&pool).await.unwrap().try_get(0).unwrap();
    pool.close().await;
    let listed = read_history(&path).await.unwrap();
    assert_eq!(count, 1);
    assert_eq!(listed.len(), 1);
    let _ = fs::remove_dir_all(&dir);
}
