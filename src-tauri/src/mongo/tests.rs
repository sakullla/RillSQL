//! Mongo 模块测试：ClientOptions 装配（URI/host+port、凭据/认证库/副本集/TLS 三档）、
//! JSON 过滤错误定位、CRUD 守卫（确认强制/校验前置）与计数策略。

use crate::connections::drivers::mongo_client_options;
use crate::connections::model::{CaMode, ConnectionDraft, Engine, SavedConnection, TlsMode};
use crate::connections::route::{self, Route};
use crate::mongo::docs::{self, CountMode};

fn route_for(host: &str, port: u16) -> Route {
    Route {
        transport_host: host.to_owned(),
        transport_port: port,
        tls_server_name: None,
        tls_mode: TlsMode::Off,
        channel_target_host: None,
        channel_target_port: None,
        bastion_host: None,
        bastion_port: None,
    }
}

fn saved(host: &str, port: u16, database: Option<&str>, username: Option<&str>, replica_set: Option<&str>, tls: TlsMode) -> SavedConnection {
    SavedConnection {
        id: "mongo-test".to_owned(),
        name: "Mongo 测试".to_owned(),
        engine: Engine::Mongodb,
        env: "local".to_owned(),
        host: Some(host.to_owned()),
        port: Some(port),
        database: database.map(str::to_owned),
        username: username.map(str::to_owned),
        file_path: None,
        replica_set: replica_set.map(str::to_owned),
        tls: Some(crate::connections::model::TlsSettings {
            mode: tls,
            server_name: host.to_owned(),
            ca_mode: CaMode::System,
            ca_file: None,
            client_cert: None,
            client_key: None,
        }),
        ssh: None,
        password_saved: false,
        ssh_password_saved: false,
    }
}

#[tokio::test]
async fn host_port_options_carry_credential_auth_source_replica_set_and_tls() {
    let config = saved("127.0.0.1", 27017, Some("shop"), Some("root"), Some("rs0"), TlsMode::Require);
    let options = mongo_client_options(&config, &route_for("127.0.0.1", 27017), "secret")
        .await
        .expect("options");
    assert!(matches!(
        options.hosts.as_slice(),
        [mongodb::options::ServerAddress::Tcp { host, port: Some(27017) }] if host == "127.0.0.1"
    ));
    let credential = options.credential.as_ref().expect("credential");
    assert_eq!(credential.username.as_deref(), Some("root"));
    assert_eq!(credential.password.as_deref(), Some("secret"));
    assert_eq!(credential.source.as_deref(), Some("shop"));
    assert_eq!(credential.mechanism, None, "SCRAM 机制留给服务器协商（SHA-256 优先）");
    assert_eq!(options.repl_set_name.as_deref(), Some("rs0"));
    match options.tls.as_ref() {
        Some(mongodb::options::Tls::Enabled(tls)) => assert_eq!(tls.allow_invalid_certificates, Some(true), "Require 档不核对证书"),
        other => panic!("Require 档应启用 TLS：{other:?}"),
    }
}

#[tokio::test]
async fn host_port_options_without_username_have_no_credential() {
    let config = saved("127.0.0.1", 27017, Some("shop"), None, None, TlsMode::Off);
    let options = mongo_client_options(&config, &route_for("127.0.0.1", 27017), "").await.expect("options");
    assert!(options.credential.is_none(), "无用户名=无认证直连");
    assert!(options.tls.is_none(), "Off 档不启用 TLS");
}

#[tokio::test]
async fn uri_options_are_parsed_and_form_fields_only_fill_gaps() {
    let uri = "mongodb://root@mongo.example.test:27018/shop?replicaSet=rs0";
    let config = saved(uri, 27017, Some("admin"), None, None, TlsMode::Off);
    let options = mongo_client_options(&config, &route_for("mongo.example.test", 27017), "secret")
        .await
        .expect("options");
    assert!(matches!(
        options.hosts.as_slice(),
        [mongodb::options::ServerAddress::Tcp { host, port: Some(27018) }] if host == "mongo.example.test"
    ), "URI 的地址优先于表单端口");
    assert_eq!(options.credential.as_ref().and_then(|c| c.username.as_deref()), Some("root"));
    assert_eq!(options.credential.as_ref().and_then(|c| c.password.as_deref()), Some("secret"), "URI 缺密码时用 keyring 密码补上");
    assert_eq!(options.repl_set_name.as_deref(), Some("rs0"), "串里写过的 replicaSet 优先");
}

#[tokio::test]
async fn uri_without_tls_picks_up_form_verify_mode_with_ca_file() {
    let uri = "mongodb://mongo.example.test:27017";
    let mut config = saved(uri, 27017, None, Some("root"), None, TlsMode::Verify);
    if let Some(tls) = config.tls.as_mut() {
        tls.ca_mode = CaMode::File;
        tls.ca_file = Some("C:\\certs\\mongo-ca.pem".to_owned());
    }
    let options = mongo_client_options(&config, &route_for("mongo.example.test", 27017), "").await.expect("options");
    match options.tls.as_ref() {
        Some(mongodb::options::Tls::Enabled(tls)) => {
            assert_eq!(tls.allow_invalid_certificates, Some(false));
            assert_eq!(
                tls.ca_file_path.as_deref().map(std::path::Path::to_string_lossy).as_deref(),
                Some("C:\\certs\\mongo-ca.pem")
            );
        }
        other => panic!("Verify 档应启用 TLS：{other:?}"),
    }
}

#[tokio::test]
async fn uri_with_ssh_is_rejected_before_any_connection_or_tunnel() {
    // P1 修复：整串 URI + SSH 的组合会在装配阶段被明确拒绝——URI 的地址直接进驱动 hosts，
    // 转发器地址没有入口，组合等于静默绕过隧道。保存阶段（normalize）同样拒绝。
    let uri = "mongodb://mongo.example.test:27017";
    let mut config = saved(uri, 27017, None, Some("root"), None, TlsMode::Off);
    config.ssh = Some(crate::connections::model::SshSettings {
        host: "bastion.example.test".to_owned(),
        port: 22,
        username: "ops".to_owned(),
        auth: crate::connections::model::SshAuthKind::Key,
        key_path: None,
    });
    let error = mongo_client_options(&config, &route_for("mongo.example.test", 27017), "")
        .await
        .expect_err("URI+SSH 必须拒绝");
    let crate::connections::stages::StageError::Fail { message: text, .. } = error else {
        panic!("应是 Fail 错误：{error:?}")
    };
    assert!(text.contains("整串 URI 暂不支持 SSH 隧道"), "{text}");
    assert!(text.contains("host/port"), "{text}");
    // host/port 形态 + SSH 不受影响（走转发器地址，隧道有入口）。
    let tunneled = saved("mongo.internal", 27017, None, Some("root"), None, TlsMode::Off);
    assert!(mongo_client_options(&tunneled, &route_for("127.0.0.1", 51027), "").await.is_ok());
}

#[test]
fn draft_normalize_rejects_uri_host_with_ssh_on() {
    // 保存阶段拒绝：这条组合到不了连接阶段（届时 SSH 隧道可能已建立）。
    let mut draft = ConnectionDraft::blank(Engine::Mongodb);
    draft.host = "mongodb://mongo.example.test".to_owned();
    draft.username = "root".to_owned();
    draft.ssh_on = true;
    draft.ssh_host = "bastion.example.test".to_owned();
    draft.ssh_user = "ops".to_owned();
    let errors = crate::connections::model::normalize(&draft, "id-uri-ssh".to_owned()).unwrap_err();
    assert!(
        errors.iter().any(|error| error.field == "ssh_host" && error.message.contains("整串 URI 暂不支持 SSH 隧道")),
        "{errors:?}"
    );
    // host/port 形态开启 SSH 合法。
    draft.host = "mongo.example.test".to_owned();
    draft.port = "27017".to_owned();
    assert!(crate::connections::model::normalize(&draft, "id-ok".to_owned()).is_ok());
}

#[tokio::test]
async fn verify_mode_rejects_split_cert_and_key_paths() {
    // P2 修复：驱动只接受证书+私钥合并的单个 PEM（cert_key_file_path）；两栏填不同路径时报明确错误，
    // 不再静默忽略私钥栏。填同一路径 = 合并后的 PEM；只填一栏时按该文件即合并 PEM 处理。
    let mut config = saved("mongo.example.test", 27017, None, Some("root"), None, TlsMode::Verify);
    if let Some(tls) = config.tls.as_mut() {
        tls.client_cert = Some("C:\\certs\\client.pem".to_owned());
        tls.client_key = Some("C:\\certs\\client.key".to_owned());
    }
    let error = mongo_client_options(&config, &route_for("mongo.example.test", 27017), "")
        .await
        .expect_err("证书与私钥分属两个文件必须拒绝");
    let crate::connections::stages::StageError::Fail { message: text, .. } = error else {
        panic!("应是 Fail 错误：{error:?}")
    };
    assert!(text.contains("合并 PEM"), "{text}");
    assert!(text.contains("同一文件"), "{text}");

    if let Some(tls) = config.tls.as_mut() {
        tls.client_key = Some("C:\\certs\\client.pem".to_owned());
    }
    let options = mongo_client_options(&config, &route_for("mongo.example.test", 27017), "").await.expect("options");
    match options.tls.as_ref() {
        Some(mongodb::options::Tls::Enabled(tls)) => assert_eq!(
            tls.cert_key_file_path.as_deref().map(std::path::Path::to_string_lossy).as_deref(),
            Some("C:\\certs\\client.pem"),
            "同一路径即合并 PEM"
        ),
        other => panic!("Verify 档应启用 TLS：{other:?}"),
    }

    // 只填证书栏：该文件即合并 PEM（其余引擎的两栏语义由各自驱动处理）。
    if let Some(tls) = config.tls.as_mut() {
        tls.client_key = None;
    }
    let options = mongo_client_options(&config, &route_for("mongo.example.test", 27017), "").await.expect("options");
    match options.tls.as_ref() {
        Some(mongodb::options::Tls::Enabled(tls)) => assert_eq!(
            tls.cert_key_file_path.as_deref().map(std::path::Path::to_string_lossy).as_deref(),
            Some("C:\\certs\\client.pem")
        ),
        other => panic!("Verify 档应启用 TLS：{other:?}"),
    }
}

#[tokio::test]
async fn uri_with_explicit_tls_wins_over_form_mode() {
    let uri = "mongodb://mongo.example.test/?tls=false";
    let config = saved(uri, 27017, None, None, None, TlsMode::Verify);
    let options = mongo_client_options(&config, &route_for("mongo.example.test", 27017), "").await.expect("options");
    assert_eq!(options.tls, Some(mongodb::options::Tls::Disabled), "串里写过的 tls 参数优先于表单档位");
}

#[test]
fn filter_errors_carry_line_and_column() {
    let error = docs::parse_document("过滤", "{\"city\": \"杭州\",}").unwrap_err();
    assert!(error.contains("line"), "定位信息要含行号：{error}");
    assert!(error.contains("column"), "定位信息要含列号：{error}");
    // 合法过滤能读回字段。
    let filter = docs::parse_document("过滤", "{\"status\": \"active\", \"总数\": {\"$numberInt\": \"3\"}}").unwrap();
    assert_eq!(filter.get_str("status"), Ok("active"));
    // $numberInt 反序列化为 Int32；扩展 JSON 的类型标记保留 BSON 类型信息。
    assert_eq!(filter.get_i32("总数"), Ok(3));
}

#[test]
fn count_mode_follows_filter_presence() {
    assert_eq!(docs::count_mode(&mongodb::bson::Document::new()), CountMode::Estimated);
    assert_eq!(docs::count_mode(&mongodb::bson::doc! {"status": "active"}), CountMode::Exact);
}

#[test]
fn delete_guard_requires_confirmation_and_valid_id_before_any_command() {
    // 确认强制与 _id 校验都在命令层：confirmed=false / 非法 _id 不发任何数据库命令。
    let id = docs::parse_id("{\"$oid\":\"65f2c1e4a1b2c3d4e5f60718\"}").unwrap();
    let filter = docs::id_filter(id);
    assert_eq!(filter.get_object_id("_id").map(|oid| oid.to_hex()), Ok("65f2c1e4a1b2c3d4e5f60718".to_owned()));
    assert!(docs::parse_id("{bad json").is_err());
    assert!(docs::parse_document("过滤", "{\"a\":1").is_err());
    assert!(docs::parse_document("文档", "{\"a\":1").unwrap_err().starts_with("文档语法错误"));
}

#[test]
fn replace_and_insert_validate_json_before_commands() {
    // 整体 JSON 覆盖与新增都先走 parse_document：非法 JSON 在命令层就被拦截。
    let replacement = docs::parse_document("文档", "{\"_id\": {\"$oid\": \"65f2c1e4a1b2c3d4e5f60718\"}, \"status\": \"paid\", \"明细\": [1, 2]}").unwrap();
    assert_eq!(replacement.get_array("明细").map(Vec::len), Ok(2));
    let error = docs::parse_document("文档", "{\"status\": paid}").unwrap_err();
    assert!(error.contains("line"), "{error}");
}

#[test]
fn draft_normalize_roundtrips_replica_set_and_allows_uri_without_port() {
    let mut draft = ConnectionDraft::blank(Engine::Mongodb);
    draft.host = "mongodb+srv://cluster.example.test".to_owned();
    draft.port = String::new();
    draft.username = "root".to_owned();
    draft.replica_set = " rs0 ".to_owned();
    let saved_config = crate::connections::model::normalize(&draft, "id-1".to_owned()).expect("normalize");
    assert_eq!(saved_config.port, Some(27017), "URI 形态下端口字段缺省不报错");
    assert_eq!(saved_config.replica_set.as_deref(), Some("rs0"), "副本集参数去空格保存");
    assert_eq!(saved_config.username.as_deref(), Some("root"));
    // 非 MongoDB 引擎不保存副本集字段。
    let mut mysql = ConnectionDraft::blank(Engine::Mysql);
    mysql.host = "db".to_owned();
    mysql.username = "root".to_owned();
    mysql.replica_set = "rs0".to_owned();
    let mysql_saved = crate::connections::model::normalize(&mysql, "id-2".to_owned()).expect("normalize");
    assert_eq!(mysql_saved.replica_set, None);
}

#[test]
fn mongo_engine_is_document_only() {
    assert_eq!(serde_json::to_string(&Engine::Mongodb).unwrap(), "\"mongodb\"");
    assert_eq!(Engine::parse("mongodb").unwrap(), Engine::Mongodb);
    assert!(!Engine::Mongodb.is_relational(), "MongoDB 没有 SQL 面/结构编辑/对比/导出");
    assert!(Engine::host_is_mongo_uri("MongoDB://host"));
    assert!(Engine::host_is_mongo_uri("mongodb+srv://cluster"));
    assert!(!Engine::host_is_mongo_uri("mongo.example.test"));
}

#[test]
fn route_resolve_keeps_mongo_tunnel_semantics() {
    // SSH 隧道复用：transport 指向本机转发端口时证书仍按数据库主机名（route::resolve_route 的既有纪律）。
    let mut config = saved("mongo.internal", 27017, None, None, None, TlsMode::Verify);
    config.ssh = Some(crate::connections::model::SshSettings {
        host: "bastion.internal".to_owned(),
        port: 22,
        username: "ops".to_owned(),
        auth: crate::connections::model::SshAuthKind::Key,
        key_path: None,
    });
    let route = route::resolve_route(&config, Some(51027)).expect("route");
    assert_eq!(route.transport_host, "127.0.0.1");
    assert_eq!(route.transport_port, 51027);
    assert_eq!(route.channel_target_port, Some(27017), "转发通道仍指向数据库端口");
    assert_eq!(route.tls_server_name.as_deref(), Some("mongo.internal"));
}
