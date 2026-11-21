//! Navicat Premium 18.0.8 emits UTF-8 NCX 1.5 with one Connection element per profile.
use roxmltree::{Document, Node};

use super::{blank, plain, valid_host, ProfileRow};
use crate::connections::model::{parse_port, ConnectionDraft, Engine, SshAuthKind, TlsMode};

pub(crate) fn parse_navicat(bytes: &[u8]) -> Result<Vec<ProfileRow>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "NCX 文件编码无效或文件已加密")?;
    let xml = Document::parse(text.trim_start_matches('\u{feff}'))
        .map_err(|_| "NCX 不是可读取的 XML 文件")?;
    let root = xml.root_element();
    if !root.has_tag_name("Connections") || root.tag_name().namespace().is_some() {
        return Err("文件缺少 Navicat Connections 根元素".into());
    }
    if root.attribute("Ver") != Some("1.5") || root.attributes().len() != 1 {
        return Err("未验证此 NCX 版本或封装格式".into());
    }
    if root
        .children()
        .any(|node| node.is_text() && node.text().is_some_and(|text| !text.trim().is_empty()))
    {
        return Err("NCX 根元素的连接内容无法识别".into());
    }
    Ok(root
        .children()
        .filter(Node::is_element)
        .enumerate()
        .map(|(index, node)| parse_row(node, index + 1))
        .collect())
}

fn field<'a>(node: Node<'a, '_>, name: &str) -> &'a str {
    node.attribute(name).unwrap_or("").trim()
}

fn secret_attribute(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "password"
            | "pwd"
            | "ssh_password"
            | "ssh_passphrase"
            | "ssh_privatekeypassphrase"
            | "ssl_keypassword"
            | "ssl_keypassphrase"
            | "http_pa_password"
            | "http_proxy_password"
            | "savepassword"
            | "ssh_savepassword"
            | "ssh_savepassphrase"
    )
}

fn parse_row(node: Node<'_, '_>, index: usize) -> ProfileRow {
    let name = field(node, "ConnectionName");
    let mut row = ProfileRow {
        name: if !name.is_empty() && plain(name) {
            name.to_owned()
        } else {
            format!("连接 {index}")
        },
        draft: None,
        blockers: Vec::new(),
        notes: Vec::new(),
        required_tls: None,
    };
    if !node.has_tag_name("Connection") || node.tag_name().namespace().is_some() {
        row.block("未识别的 NCX 连接条目");
        return row;
    }
    let engine = match field(node, "ConnType") {
        "MYSQL" => Engine::Mysql,
        "POSTGRESQL" => Engine::Postgres,
        "SQLITE" => Engine::Sqlite,
        _ => {
            row.block("未验证此 NCX 引擎类型");
            return row;
        }
    };
    let mut draft = blank(engine, &row.name);
    if node.children().any(|child| {
        child.is_element()
            || child.is_text() && child.text().is_some_and(|text| !text.trim().is_empty())
    }) {
        row.block("无法还原 NCX 嵌套的连接设置");
    }
    let user = field(node, "UserName");
    if plain(user) {
        draft.username = user.to_owned();
    }
    if engine == Engine::Sqlite {
        let path = field(node, "DatabaseFileName");
        if path.starts_with("file:") || path == ":memory:" || path.contains('?') {
            row.block("无法还原 SQLite URI 的文件或访问模式");
        } else if plain(path) {
            draft.file_path = path.to_owned();
        }
        if !field(node, "CipherName").is_empty() {
            row.block("无法还原 SQLite 的加密设置");
        }
    } else {
        let host = field(node, "Host");
        if valid_host(host) {
            draft.host = host.to_owned();
        }
        draft.port = parse_port(field(node, "Port"))
            .map(|port| port.to_string())
            .unwrap_or_default();
        let db = field(node, "Database");
        if plain(db) {
            draft.database = db.to_owned();
        } else {
            row.block("NCX 的数据库字段无法安全还原");
        }
    }
    match field(node, "HTTP") {
        "false" => (),
        "true" => row.block("NCX 要求 HTTP 隧道，当前无法保留此路由"),
        _ => row.block("无法确定 NCX 的 HTTP 隧道状态"),
    }
    match (engine, field(node, "SSL")) {
        (Engine::Sqlite, "") | (_, "false") => {
            draft.tls_mode = TlsMode::Off;
            row.required_tls = Some(TlsMode::Off);
        }
        (Engine::Postgres, "true") => {
            row.block("NCX 要求 PostgreSQL TLS，当前驱动在服务器拒绝 TLS 时可能回退到明文")
        }
        (_, "true") => row.block("未验证此 NCX 引擎的 SSL 模式和证书校验选项"),
        _ => row.block("无法确定 NCX 的 SSL 校验配置"),
    }
    match (engine, field(node, "SSH")) {
        (Engine::Sqlite, "") | (_, "false") => (),
        (Engine::Mysql, "true") => apply_ssh(node, &mut draft, &mut row),
        (_, "true") => row.block("未验证此 NCX 引擎的 SSH 设置，无法保留路由"),
        _ => row.block("无法确定 NCX 的 SSH 路由状态"),
    }

    for attribute in node.attributes() {
        if attribute.namespace().is_some() {
            row.block("无法解释 NCX 带命名空间的连接属性");
            continue;
        }
        let key = attribute.name();
        let value = attribute.value();
        match key {
            "ConnectionName"
            | "ConnType"
            | "Host"
            | "Port"
            | "UserName"
            | "DatabaseFileName"
            | "Database"
            | "SSL"
            | "HTTP"
            | "SSH"
            | "CipherName"
            | "SettingsSavePath"
            | "Remarks"
            | "ProjectUUID"
            | "SessionLimit"
            | "Keepalive"
            | "UseConnectionTimeout"
            | "ConnectionTimeoutSeconds"
            | "UseReadTimeout"
            | "ReadTimeoutSeconds"
            | "UseWriteTimeout"
            | "WriteTimeoutSeconds"
            | "AutoConnect" => (),
            "ServiceProvider" | "ClientDriverVersion" | "HostTypePreference"
                if value == "Default" =>
            {
                ()
            }
            "Encoding" | "ClientEncoding" if value == "65001" => (),
            "ClientCharacterSet" | "ClientDriverEncoding" if value.is_empty() => (),
            "MySQLCharacterSet" if value == "true" => (),
            "Compression" | "Compatibility" | "NamedPipe" | "UseAdvanced" if value == "false" => (),
            "CipherLegacyPageSize" if value == "0" => (),
            "SSH_Host" | "SSH_Port" | "SSH_UserName" | "SSH_PrivateKey" | "SSH_AuthenMethod"
                if engine == Engine::Mysql =>
            {
                ()
            }
            key if key.starts_with("HTTP_") && field(node, "HTTP") == "true" => (),
            "ConnectionParameters" | "InitialSessionQueries" if value.is_empty() => (),
            "ConnectionParameters" => row.block("NCX 额外连接参数可能改变目标或访问条件"),
            "InitialSessionQueries" => row.block("无法还原 NCX 启动会话语句的访问条件"),
            "ReadOnly" if value == "false" => (),
            "ReadOnly" => row.block("NCX 指定只读或只读状态不明，无法保证禁止写入"),
            key if secret_attribute(key) => (),
            _ => row.block("条目含未验证的 NCX 配置或访问选项"),
        }
    }
    row.draft = Some(draft);
    row
}

fn apply_ssh(node: Node<'_, '_>, draft: &mut ConnectionDraft, row: &mut ProfileRow) {
    draft.ssh_on = true;
    let host = field(node, "SSH_Host");
    if valid_host(host) {
        draft.ssh_host = host.to_owned();
    }
    draft.ssh_port = parse_port(field(node, "SSH_Port"))
        .map(|p| p.to_string())
        .unwrap_or_default();
    let user = field(node, "SSH_UserName");
    if plain(user) {
        draft.ssh_user = user.to_owned();
    }
    draft.ssh_auth = SshAuthKind::Password;
    if field(node, "SSH_AuthenMethod") != "PASSWORD" || !field(node, "SSH_PrivateKey").is_empty() {
        row.block("未验证此 NCX SSH 认证方式，无法保留认证条件");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(rows: &str) -> Vec<ProfileRow> {
        parse_navicat(format!("<Connections Ver=\"1.5\">{rows}</Connections>").as_bytes()).unwrap()
    }

    #[test]
    fn mixed_ncx_keeps_targets_routes_and_discards_credentials() {
        let rows = export(
            r#"
            <Connection ConnectionName="PG" ConnType="POSTGRESQL" ServiceProvider="Default" Host="127.0.0.1" Port="5432" Database="reports" UserName="reader" Password="ncx-db-secret" UseAdvanced="false" SSL="false" SSH="false" HTTP="false"/>
            <Connection ConnectionName="local" ConnType="SQLITE" DatabaseFileName="relative.sqlite" CipherName="" HTTP="false"/>
            <Connection ConnectionName="proxied" ConnType="MYSQL" Host="db.example" Port="3306" UserName="reader" HTTP="true" HTTP_URL="https://ncx-http-secret@invalid/tunnel.php" SSH="false" SSL="false"/>
            <Connection ConnectionName="tunnel" ConnType="MYSQL" Host="db.internal" Port="3307" UserName="dbuser" SSL="false" HTTP="false" SSH="true" SSH_Host="bastion.example" SSH_Port="2222" SSH_UserName="relay" SSH_AuthenMethod="PASSWORD" SSH_PrivateKey="" SSH_Password="ncx-ssh-secret" SSH_Passphrase="ncx-private-secret"/>
        "#,
        );
        assert_eq!(rows.len(), 4);
        let pg = rows[0].draft.as_ref().unwrap();
        assert!(rows[0].issues(pg).is_empty());
        assert_eq!(
            (&*pg.host, &*pg.database, pg.tls_mode),
            ("127.0.0.1", "reports", TlsMode::Off)
        );
        let sqlite = rows[1].draft.as_ref().unwrap();
        assert_eq!(sqlite.file_path, "relative.sqlite");
        assert!(rows[1]
            .issues(sqlite)
            .iter()
            .any(|i| i.field == "file_path"));
        assert!(rows[2].blockers.iter().any(|msg| msg.contains("HTTP")));
        let tunnel = rows[3].draft.as_ref().unwrap();
        assert!(rows[3].issues(tunnel).is_empty());
        assert!(tunnel.ssh_on);
        assert_eq!(
            (
                &*tunnel.host,
                &*tunnel.ssh_host,
                &*tunnel.ssh_port,
                &*tunnel.ssh_user
            ),
            ("db.internal", "bastion.example", "2222", "relay")
        );
        let mut edited = tunnel.clone();
        edited.ssh_on = false;
        assert!(!rows[3].issues(&edited).is_empty());
        assert!(!format!("{rows:?}").contains("ncx-"));
    }

    #[test]
    fn unsupported_access_conditions_keep_row_blocked() {
        for extra in [
            r#"ConnectionParameters="host=other""#,
            r#"ReadOnly="true""#,
            r#"UseAdvanced="true""#,
            r#"UnverifiedProxy="true""#,
        ] {
            let rows = export(&format!(
                r#"<Connection ConnType="POSTGRESQL" Host="h" Port="5432" UserName="r" SSH="false" HTTP="false" SSL="false" {extra}/>"#
            ));
            let draft = rows[0].draft.as_ref().unwrap();
            assert!(!rows[0].issues(draft).is_empty());
        }
        let rows = export(
            r#"<Connection ConnType="POSTGRESQL" SSL="true" HTTP="false" SSH="false"/><Connection ConnType="unk"/>"#,
        );
        assert!(rows[0].blockers.iter().any(|msg| msg.contains("明文")));
        assert!(rows[1].draft.is_none());
    }

    #[test]
    fn broken_xml_unknown_container_and_unverified_version_are_file_errors() {
        for file in [
            "",
            "<Connections",
            "<config/>",
            "<Connections Ver='2.0'/>",
            "<Connections Ver='1.5'>ciphertext</Connections>",
        ] {
            assert!(parse_navicat(file.as_bytes()).is_err());
        }
        assert!(parse_navicat(b"<Connections Ver='1.5'/>")
            .unwrap()
            .is_empty());
    }
}
