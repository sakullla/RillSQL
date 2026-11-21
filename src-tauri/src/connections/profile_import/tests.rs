use super::*;
use serde_json::{json, Value};

fn parse(file: Value) -> Vec<ProfileRow> {
    parse_dbeaver(&serde_json::to_vec(&file).unwrap()).unwrap()
}

fn postgres(config: Value) -> Value {
    json!({"name":"Reports","provider":"postgresql","driver":"postgres-jdbc","configuration":config})
}

fn single(config: Value) -> ProfileRow {
    parse(json!({"connections":{"one":postgres(config)}}))
        .pop()
        .unwrap()
}

#[test]
fn separates_file_errors_and_row_failures() {
    assert!(parse_dbeaver(br#"{"connections":["a"]}"#).is_err());
    assert!(parse_dbeaver(b"secret: invalid").is_err());
    let rows = parse(json!({"connections":{
        "a":postgres(json!({"host":"pg.example","user":"reader","database":"app","port":5433})),
        "b":{"name":"unknown","provider":"generic","driver":"other","configuration":{}},
        "c":42}}));
    assert_eq!(rows.len(), 3);
    let draft = rows[0].draft.as_ref().unwrap();
    assert_eq!(
        (&*draft.host, &*draft.port, &*draft.database),
        ("pg.example", "5433", "app")
    );
    assert!(rows[0].issues(draft).is_empty());
    assert!(rows[1].blockers[0].contains("驱动"));
    assert!(rows[2].blockers[0].contains("格式"));
}

#[test]
fn url_target_wins_and_no_password_or_raw_uri_survives() {
    let row = single(
        json!({"host":"fallback","port":"9","database":"wrong","user":"legacy",
        "url":"jdbc:postgresql://actual:test-db-pass@db.internal:6543/rep%6Frts?sslmode=verify-full&password=test-url-pass",
        "password":"test-plain-pass",
        "handlers":{"ssh_tunnel":{"type":"TUNNEL","enabled":true,"user":"relay",
            "password":"test-ssh-pass","properties":{"host":"bastion","authType":"PASSWORD","passphrase":"test-key-pass"}}}}),
    );
    let draft = row.draft.as_ref().unwrap();
    assert!(row.issues(draft).is_empty(), "{:?}", row.issues(draft));
    assert_eq!(
        (
            &*draft.host,
            &*draft.port,
            &*draft.database,
            &*draft.username
        ),
        ("db.internal", "6543", "reports", "actual")
    );
    assert!(draft.ssh_on);
    assert_eq!(draft.tls_mode, TlsMode::Verify);
    let rendered = format!("{row:?} {}", serde_json::to_string(draft).unwrap());
    for secret in [
        "jdbc:",
        "test-db-pass",
        "test-url-pass",
        "test-plain-pass",
        "test-ssh-pass",
        "test-key-pass",
    ] {
        assert!(!rendered.contains(secret));
    }
    let mut edit = draft.clone();
    edit.ssh_on = false;
    edit.tls_mode = TlsMode::Off;
    assert_eq!(row.issues(&edit).len(), 2);
}

#[test]
fn unsupported_urls_never_fall_back_or_reveal_the_input() {
    for url in [
        "jdbc:oracle://user:sensitive@ignored",
        "jdbc:postgresql://first,second/sensitive",
        "jdbc:postgresql://host/db?targetServerType=sensitive",
        "jdbc:postgresql://${sensitive}/db",
        "jdbc:postgresql://user:sensitive@host:b/db",
        "jdbc:postgresql://host/%ZZ?password=sensitive",
    ] {
        let row = single(json!({"host":"fallback","user":"viewer","url":url}));
        assert!(!row.blockers.is_empty());
        let text = format!("{row:?}");
        assert!(
            !text.contains("sensitive") && !text.contains("fallback"),
            "{text}"
        );
    }
}

#[test]
fn sqlite_paths_are_explicit_and_relative_paths_can_be_repaired() {
    let absolute = std::env::temp_dir()
        .join("rill-profile.db")
        .to_string_lossy()
        .into_owned();
    for cfg in [
        json!({"database":absolute}),
        json!({"database":"incorrect","url":format!("jdbc:sqlite:{absolute}"),"configurationType":"MANUAL"}),
    ] {
        let rows = parse(
            json!({"connections":{"x":{"name":"local","provider":"sqlite","driver":"sqlite_jdbc","configuration":cfg}}}),
        );
        let draft = rows[0].draft.as_ref().unwrap();
        assert_eq!(draft.file_path, absolute);
        assert!(rows[0].issues(draft).is_empty());
    }
    let rows = parse(
        json!({"connections":{"x":{"name":"local","provider":"sqlite","driver":"sqlite_jdbc","configuration":{"database":"relative.db"}}}}),
    );
    let mut draft = rows[0].draft.clone().unwrap();
    assert!(!rows[0].issues(&draft).is_empty());
    draft.file_path = absolute;
    assert!(rows[0].issues(&draft).is_empty());
}

#[test]
fn checks_inherited_restrictions_and_missing_ssh_identity() {
    let config = json!({"host":"db","user":"app","type":"prod",
        "provider-properties":{"@dbeaver-show-non-default-db@":"false"},
        "handlers":{"ssh_tunnel":{"type":"TUNNEL","enabled":true,"save-password":true,
        "properties":{"host":"bastion","port":22,"authType":"PASSWORD","jumpServer.count":0,
            "implementation":"sshj","shareTunnels":true,"localHost":"","remoteHost":""}}}});
    let rows = parse(json!({"connections":{"p":postgres(config.clone())},
        "connection-types":{"prod":{"security":{"permission-restrictions":["data-edit"]}}}}));
    assert!(rows[0]
        .blockers
        .iter()
        .any(|reason| reason.contains("权限")));
    let mut readonly = postgres(config.clone());
    readonly["read-only"] = true.into();
    assert!(parse(json!({"connections":{"p":readonly}}))[0]
        .blockers
        .iter()
        .any(|reason| reason.contains("只读")));
    let row = single(config);
    let mut draft = row.draft.clone().unwrap();
    assert_eq!(draft.env, "prod");
    assert!(row.issues(&draft).iter().any(|e| e.field == "ssh_user"));
    draft.ssh_user = "tunnel-user".into();
    assert!(row.issues(&draft).is_empty());
}

#[test]
fn cert_rules_are_preserved_and_unknown_network_routes_are_blocked() {
    let row = single(
        json!({"host":"db","user":"reader","handlers":{"postgre_ssl":{"enabled":true,
        "properties":{"ssl.sslMode":"verify-full","ssl.ca.cert":"${project.path}/ca.crt"}}}}),
    );
    let mut draft = row.draft.clone().unwrap();
    assert_eq!(draft.ca_mode, CaMode::File);
    assert!(row.issues(&draft).iter().any(|e| e.field == "ca_file"));
    draft.ca_file = std::env::temp_dir()
        .join("ca.crt")
        .to_string_lossy()
        .into_owned();
    assert!(row.issues(&draft).is_empty());
    for cfg in [
        json!({"properties":{"sslmode":"verify-ca"}}),
        json!({"handlers":{"proxy":{"enabled":true}}}),
        json!({"config-profile":"VPN"}),
        json!({"handlers":{"ssh_tunnel":{"enabled":true,"properties":{"authType":"PASSWORD","host":"bastion","jumpServer.count":2}}}}),
        json!({"bootstrap":{"query":["set role restricted"]}}),
    ] {
        assert!(!single(cfg).blockers.is_empty());
    }
}

#[test]
fn allows_completion_of_driver_defaults_but_never_weakens_an_explicit_policy() {
    let row = single(json!({"host":"local","port":"5432","database":"app"}));
    let mut filled = row.draft.clone().unwrap();
    assert!(row
        .issues(&filled)
        .iter()
        .any(|issue| issue.field == "username"));
    filled.username = "test".into();
    filled.tls_mode = TlsMode::Off;
    assert!(row.issues(&filled).is_empty());
    let row =
        single(json!({"url":"jdbc:postgresql://local/app?sslmode=verify-full","user":"test"}));
    assert!(row
        .issues(&filled)
        .iter()
        .any(|issue| issue.field == "tls_mode"));
}

#[test]
fn blocks_readonly_file_uri_without_jdbc_prefix_and_handles_bad_field_types() {
    let rows = parse(
        json!({"connections":{"s":{"provider":"sqlite","driver":"sqlite_jdbc",
        "configuration":{"database":"file:reports.db?mode=ro"}}}}),
    );
    assert!(rows[0]
        .blockers
        .iter()
        .any(|reason| reason.contains("访问模式")));
    for bad_database in [
        json!(42),
        json!("${tenant_id}"),
        json!("jdbc:postgresql://u:secret@host/db"),
    ] {
        let row = single(json!({"database":bad_database,"host":"h","user":"u"}));
        assert!(!row.blockers.is_empty());
        assert!(!format!("{row:?}").contains("secret"));
    }
}
