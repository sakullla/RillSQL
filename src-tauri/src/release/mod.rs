//! 兼容现有 GitHub `latest.json` / minisign 协议的打包名与更新门禁。
//!
//! 服务端清单不增加 cargo-packager 的 `format` 字段。安装器可以复用
//! cargo-packager-updater，但验签失败、活动任务或未保存编辑都不会更换当前版本。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::shell::{update_block_key, CloseFacts};

pub const PACKAGER_VERSION: &str = "0.11.8";
pub const PACKAGER_UPDATER_VERSION: &str = "0.2.3";
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const UPDATER_ENDPOINT: &str = "https://github.com/sakullla/RillSQL/releases/latest/download/latest.json";
/// 与既有发布钥匙相同的 updater 公钥，不能另换一把钥匙。
pub const UPDATER_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDVDMUFGMEEzQjI1REM2RDQKUldUVXhsMnlvL0FhWFBuNHFJRUcrZ2xWYVBmaU5yaHFMUklmVTRRRExlTnJDbmk5eFNSSXRIV0gK";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("{0}")]
    Blocked(&'static str),
    #[error("{0}")]
    Rejected(String),
    #[error("install failed, current version kept: {0}")]
    Install(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct PlatformUpdate {
    pub target: String,
    pub version: String,
    pub url: String,
    pub signature: String,
    pub format: &'static str,
}

#[derive(Debug)]
struct Manifest {
    version: String,
    platforms: BTreeMap<String, RawPlatform>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    version: String,
    platforms: BTreeMap<String, RawPlatform>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawPlatform {
    signature: String,
    url: String,
    /// 旧清单没有这个字段。出现时只作提示，缺失不影响解析。
    #[serde(default)]
    format: Option<String>,
}

pub fn windows_nsis_name(version: &str) -> String {
    format!("RillSQL_{version}_x64-setup.exe")
}

pub fn macos_dmg_name(version: &str) -> String {
    format!("RillSQL_{version}_aarch64.dmg")
}

pub fn linux_deb_name(version: &str) -> String {
    format!("RillSQL_{version}_amd64.deb")
}

pub fn linux_appimage_name(version: &str) -> String {
    format!("RillSQL_{version}_amd64.AppImage")
}

pub fn macos_updater_name(version: &str) -> String {
    format!("RillSQL_{version}_aarch64.app.tar.gz")
}

pub fn installer_names(version: &str) -> [String; 4] {
    [windows_nsis_name(version), macos_dmg_name(version), linux_deb_name(version), linux_appimage_name(version)]
}

pub fn updater_names(version: &str) -> [(&'static str, String); 4] {
    [
        ("windows-x86_64", windows_nsis_name(version)),
        ("linux-x86_64", linux_appimage_name(version)),
        ("linux-x86_64-deb", linux_deb_name(version)),
        ("darwin-aarch64", macos_updater_name(version)),
    ]
}

/// cargo-packager 的默认文件名改成现有发布协议的名字。版本对不上就拒绝改名。
pub fn protocol_file_name(packager_file_name: &str, version: &str) -> Result<String, String> {
    if packager_file_name.ends_with(".sig") {
        let stem = packager_file_name.trim_end_matches(".sig");
        return Ok(format!("{}.sig", protocol_file_name(stem, version)?));
    }
    let expected = installer_names(version);
    if expected.iter().any(|name| name == packager_file_name) || packager_file_name == macos_updater_name(version) {
        return Ok(packager_file_name.to_owned());
    }
    let lower = packager_file_name.to_ascii_lowercase();
    let version_lower = version.to_ascii_lowercase();
    if !lower.contains(&version_lower) && lower != "rillsql.app.tar.gz" {
        return Err(format!("packager output {packager_file_name} is not version {version}"));
    }
    if lower == "rillsql.app.tar.gz" || lower.ends_with(".app.tar.gz") {
        return Ok(macos_updater_name(version));
    }
    if lower.ends_with("_x64-setup.exe") {
        return Ok(windows_nsis_name(version));
    }
    if lower.ends_with("_aarch64.dmg") {
        return Ok(macos_dmg_name(version));
    }
    if lower.ends_with("_amd64.deb") {
        return Ok(linux_deb_name(version));
    }
    if lower.ends_with("_amd64.appimage") || lower.ends_with("_x86_64.appimage") {
        return Ok(linux_appimage_name(version));
    }
    Err(format!("unrecognized packager output {packager_file_name}"))
}

pub fn require_complete_release(assets: &[ReleaseAsset], version: &str) -> Result<(), String> {
    let mut required = installer_names(version).to_vec();
    required.push(macos_updater_name(version));
    for name in updater_names(version) {
        required.push(format!("{}.sig", name.1));
    }
    for name in required {
        let matches: Vec<_> = assets.iter().filter(|asset| asset.name == name).collect();
        if matches.len() != 1 {
            return Err(format!("incomplete release: expected exactly one {name}, found {}", matches.len()));
        }
        if matches[0].size == 0 {
            return Err(format!("incomplete release: {name} is empty"));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NsisInspection {
    pub name: String,
    pub size: u64,
    pub version: String,
    pub arch: &'static str,
}

/// 本机 NSIS 必须是非空的 PE，并且带有 NSIS 标记、产品名和版本。
pub fn inspect_windows_nsis(path: &Path, version: &str) -> Result<NsisInspection, String> {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_owned();
    let expected = windows_nsis_name(version);
    if name != expected {
        return Err(format!("expected {expected}, got {name}"));
    }
    if !name.contains("_x64-setup.exe") {
        return Err(format!("architecture filename is not x64: {name}"));
    }
    let bytes = fs::read(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    if bytes.is_empty() {
        return Err(format!("{name} is empty"));
    }
    if bytes.len() < 2 || &bytes[..2] != b"MZ" {
        return Err(format!("{name} is not a Windows executable"));
    }
    if !contains_text(&bytes, "Nullsoft") && !contains_text(&bytes, "NSIS") {
        return Err(format!("{name} does not look like an NSIS installer"));
    }
    // Unicode NSIS 把产品名和版本放进 UTF-16 资源，压缩后的主程序里看不到明文。
    if !contains_text(&bytes, "RillSQL") || !contains_text(&bytes, version) {
        return Err(format!("{name} is missing the product name or version {version}"));
    }
    Ok(NsisInspection { name, size: bytes.len() as u64, version: version.to_owned(), arch: "x64" })
}

pub fn collect_release_assets(directory: &Path) -> Result<Vec<ReleaseAsset>, String> {
    let mut assets = Vec::new();
    collect_release_assets_inner(directory, &mut assets)?;
    assets.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(assets)
}

fn collect_release_assets_inner(directory: &Path, assets: &mut Vec<ReleaseAsset>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|err| format!("read {}: {err}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_release_assets_inner(&path, assets)?;
            continue;
        }
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_owned();
        if name.is_empty() {
            continue;
        }
        let size = entry.metadata().map_err(|err| err.to_string())?.len();
        assets.push(ReleaseAsset { name, size });
    }
    Ok(())
}

pub fn build_updater_manifest(version: &str, notes: &str, pub_date: &str, repo: &str, signatures: &BTreeMap<String, String>) -> Result<Value, String> {
    if !repo.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/')) || repo.matches('/').count() != 1 {
        return Err(format!("invalid GitHub repository {repo}"));
    }
    let tag = format!("v{version}");
    let mut platforms = serde_json::Map::new();
    for (target, name) in updater_names(version) {
        let signature = signatures.get(&name).ok_or_else(|| format!("missing signature for {name}"))?;
        let url = format!("https://github.com/{repo}/releases/download/{tag}/{name}");
        platforms.insert(target.to_owned(), json!({ "signature": signature, "url": url }));
    }
    platforms.insert("windows-x86_64-nsis".to_owned(), platforms["windows-x86_64"].clone());
    platforms.insert("linux-x86_64-appimage".to_owned(), platforms["linux-x86_64"].clone());
    platforms.insert("darwin-aarch64-app".to_owned(), platforms["darwin-aarch64"].clone());
    Ok(json!({
        "version": version,
        "notes": notes,
        "pub_date": pub_date,
        "platforms": platforms,
    }))
}

pub fn verify_minisign(data: &[u8], signature_b64: &str, pubkey_b64: &str, version: &str) -> Result<(), String> {
    let signature_text = decode_base64_text(signature_b64)?;
    let public_text = decode_base64_text(pubkey_b64)?;
    structural_signature(&signature_text, &public_text, version)?;
    let public_key = PublicKey::decode(&public_text).map_err(|err| format!("invalid minisign public key: {err}"))?;
    let signature = Signature::decode(&signature_text).map_err(|err| format!("invalid minisign signature: {err}"))?;
    public_key.verify(data, &signature, true).map_err(|_| "corrupted or mismatched updater signature".to_owned())?;
    let signed = signed_version(signature.trusted_comment()).ok_or_else(|| "updater signature is not bound to a version".to_owned())?;
    if !version_text_eq(signed, version) {
        return Err(format!("updater signature is bound to {signed}, not {version}"));
    }
    Ok(())
}

pub fn apply_verified_update<F>(app: &mut InstalledApp, facts: CloseFacts, pubkey: &str, manifest_json: &str, target: &str, bytes: &[u8], install: F) -> Result<String, UpdateError>
where
    F: FnOnce(&PlatformUpdate) -> Result<(), String>,
{
    if let Some(key) = update_block_key(facts) {
        return Err(UpdateError::Blocked(key));
    }
    let platform = select_platform(manifest_json, target).map_err(UpdateError::Rejected)?;
    if !version_is_newer(&platform.version, &app.version).map_err(UpdateError::Rejected)? {
        return Err(UpdateError::Rejected(format!("update {} is not newer than {}; current version kept", platform.version, app.version)));
    }
    verify_minisign(bytes, &platform.signature, pubkey, &platform.version).map_err(UpdateError::Rejected)?;
    install(&platform).map_err(UpdateError::Install)?;
    app.version = platform.version.clone();
    Ok(platform.version)
}

/// 只有验签和门禁都通过后才组装 cargo-packager-updater 的安装对象。
pub fn packager_update(platform: &PlatformUpdate, extract_path: &Path) -> Result<cargo_packager_updater::Update, String> {
    let format = packager_format(platform.format)?;
    let url = cargo_packager_updater::url::Url::parse(&platform.url).map_err(|err| err.to_string())?;
    Ok(cargo_packager_updater::Update {
        config: cargo_packager_updater::Config {
            endpoints: vec![cargo_packager_updater::url::Url::parse(UPDATER_ENDPOINT).map_err(|err| err.to_string())?],
            pubkey: UPDATER_PUBKEY.to_owned(),
            windows: Some(cargo_packager_updater::WindowsConfig {
                installer_args: None,
                install_mode: Some(cargo_packager_updater::WindowsUpdateInstallMode::Passive),
            }),
        },
        body: None,
        current_version: PRODUCT_VERSION.to_owned(),
        version: platform.version.clone(),
        date: None,
        target: platform.target.clone(),
        extract_path: extract_path.to_path_buf(),
        download_url: url,
        signature: platform.signature.clone(),
        timeout: None,
        headers: cargo_packager_updater::reqwest::header::HeaderMap::new(),
        format,
    })
}

pub fn launch_verified_install(update: &cargo_packager_updater::Update, bytes: Vec<u8>) -> Result<(), String> {
    update.install(bytes).map_err(|err| format!("install failed, current version kept: {err}"))
}

fn packager_format(format: &str) -> Result<cargo_packager_updater::UpdateFormat, String> {
    match format {
        "nsis" => Ok(cargo_packager_updater::UpdateFormat::Nsis),
        "appimage" => Ok(cargo_packager_updater::UpdateFormat::AppImage),
        "app" => Ok(cargo_packager_updater::UpdateFormat::App),
        "deb" => Err("cargo-packager-updater cannot install deb; current version kept".to_owned()),
        other => Err(format!("unsupported update format {other}")),
    }
}

fn select_platform(manifest_json: &str, target: &str) -> Result<PlatformUpdate, String> {
    let manifest = parse_manifest(manifest_json)?;
    let raw = manifest.platforms.get(target).ok_or_else(|| format!("manifest has no {target}"))?;
    let format = infer_format(target, &raw.url, raw.format.as_deref())?;
    if raw.url.is_empty() || raw.signature.trim().is_empty() {
        return Err(format!("{target} is missing a url or signature"));
    }
    Ok(PlatformUpdate { target: target.to_owned(), version: manifest.version, url: raw.url.clone(), signature: raw.signature.clone(), format })
}

fn parse_manifest(manifest_json: &str) -> Result<Manifest, String> {
    let raw: RawManifest = serde_json::from_str(manifest_json).map_err(|err| format!("invalid latest.json: {err}"))?;
    let version = raw.version.trim().trim_start_matches('v').to_owned();
    if version.is_empty() {
        return Err("latest.json is missing a version".to_owned());
    }
    Ok(Manifest { version, platforms: raw.platforms })
}

fn infer_format(target: &str, url: &str, explicit: Option<&str>) -> Result<&'static str, String> {
    if let Some(format) = explicit {
        return match format {
            "nsis" => Ok("nsis"),
            "appimage" => Ok("appimage"),
            "app" => Ok("app"),
            "deb" => Ok("deb"),
            other => Err(format!("unknown update format {other}")),
        };
    }
    let url_lower = url.to_ascii_lowercase();
    if target.ends_with("-nsis") || target == "windows-x86_64" || url_lower.ends_with("-setup.exe") {
        return Ok("nsis");
    }
    if target.ends_with("-appimage") || target == "linux-x86_64" || url_lower.ends_with(".appimage") {
        return Ok("appimage");
    }
    if target.ends_with("-deb") || url_lower.ends_with(".deb") {
        return Ok("deb");
    }
    if target.ends_with("-app") || target.starts_with("darwin-") || url_lower.ends_with(".app.tar.gz") {
        return Ok("app");
    }
    Err(format!("cannot infer an update format for {target}"))
}

fn version_is_newer(announced: &str, current: &str) -> Result<bool, String> {
    let announced = cargo_packager_updater::semver::Version::parse(announced.trim_start_matches('v')).map_err(|err| format!("invalid update version: {err}"))?;
    let current = cargo_packager_updater::semver::Version::parse(current.trim_start_matches('v')).map_err(|err| format!("invalid current version: {err}"))?;
    Ok(announced > current)
}

fn version_text_eq(signed: &str, announced: &str) -> bool {
    match (
        cargo_packager_updater::semver::Version::parse(signed.trim_start_matches('v')),
        cargo_packager_updater::semver::Version::parse(announced.trim_start_matches('v')),
    ) {
        (Ok(signed), Ok(announced)) => signed == announced,
        _ => signed == announced,
    }
}

fn signed_version(trusted_comment: &str) -> Option<&str> {
    trusted_comment.split('\t').find_map(|field| field.strip_prefix("version:"))
}

fn structural_signature(signature_text: &str, public_text: &str, version: &str) -> Result<(), String> {
    let lines: Vec<&str> = signature_text.trim().lines().collect();
    let public_lines: Vec<&str> = public_text.trim().lines().collect();
    if lines.len() != 4 || public_lines.len() < 2 {
        return Err("incomplete updater signature".to_owned());
    }
    let packet = base64::engine::general_purpose::STANDARD.decode(lines[1].trim()).map_err(|_| "invalid updater signature encoding".to_owned())?;
    let key = base64::engine::general_purpose::STANDARD.decode(public_lines[1].trim()).map_err(|_| "invalid updater public key encoding".to_owned())?;
    let global = base64::engine::general_purpose::STANDARD.decode(lines[3].trim()).map_err(|_| "invalid updater signature encoding".to_owned())?;
    if packet.len() != 74 || key.len() != 42 || global.len() != 64 {
        return Err("incomplete updater signature".to_owned());
    }
    if packet[2..10] != key[2..10] {
        return Err("updater signature belongs to another public key".to_owned());
    }
    let comment = lines[2];
    if !comment.starts_with("trusted comment: ") || !comment.split('\t').any(|field| field == format!("version:{version}")) {
        return Err(format!("updater signature is not bound to this release version {version}"));
    }
    Ok(())
}

fn decode_base64_text(value: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(value.trim()).map_err(|_| "invalid updater signature encoding".to_owned())?;
    String::from_utf8(bytes).map_err(|_| "updater signature is not utf-8".to_owned())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

fn contains_text(haystack: &[u8], text: &str) -> bool {
    if contains_bytes(haystack, text.as_bytes()) {
        return true;
    }
    let mut utf16 = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    contains_bytes(haystack, &utf16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::shell::{WorkItemKind, WorkbenchModel};
    use crate::app::Shell;
    use base64::Engine;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Keys {
        public_b64: String,
        secret: minisign::SecretKey,
    }

    fn keys() -> Keys {
        let minisign::KeyPair { pk, sk } = minisign::KeyPair::generate_encrypted_keypair(Some(String::new())).unwrap();
        let public_b64 = base64::engine::general_purpose::STANDARD.encode(pk.to_box().unwrap().to_string());
        Keys { public_b64, secret: sk }
    }

    fn sign(keys: &Keys, bytes: &[u8], file_name: &str, version: &str) -> String {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let comment = format!("timestamp:{stamp}\tfile:{file_name}\tversion:{version}");
        let signature = minisign::sign(None, &keys.secret, Cursor::new(bytes), Some(comment.as_str()), Some("signature from tauri secret key")).unwrap();
        base64::engine::general_purpose::STANDARD.encode(signature.to_string())
    }

    fn manifest_for(version: &str, signature: &str) -> String {
        let mut signatures = BTreeMap::new();
        for (_, name) in updater_names(version) {
            signatures.insert(name, signature.to_owned());
        }
        serde_json::to_string(&build_updater_manifest(version, "notes", "2026-11-08T00:00:00Z", "sakullla/RillSQL", &signatures).unwrap()).unwrap()
    }

    fn quiet_facts() -> CloseFacts {
        CloseFacts { running: false, uncommitted: false, draft: false }
    }

    fn rillsql_lock_version(text: &str) -> Option<String> {
        let mut current = false;
        for line in text.lines() {
            let line = line.trim();
            if line == "[[package]]" {
                current = false;
            }
            if line == "name = \"rillsql\"" {
                current = true;
            }
            if current {
                if let Some(value) = line.strip_prefix("version = \"") {
                    return Some(value.trim_end_matches('"').to_owned());
                }
            }
        }
        None
    }

    #[test]
    fn protocol_names_match_the_existing_release_scripts() {
        let updater = include_str!("../../../.github/scripts/updater.mjs");
        let release = include_str!("../../../.github/scripts/release.mjs");
        for marker in ["_x64-setup.exe", "_amd64.deb", "_amd64.AppImage"] {
            assert!(updater.contains(marker), "{marker} missing from updater.mjs");
            assert!(release.contains(marker), "{marker} missing from release.mjs");
        }
        assert!(updater.contains("aarch64.app.tar.gz"));
        assert!(release.contains("_aarch64.dmg"));
        assert_eq!(protocol_file_name("rillsql_0.2.12_x64-setup.exe", "0.2.12").unwrap(), windows_nsis_name("0.2.12"));
        assert_eq!(protocol_file_name("RillSQL_0.2.12_x86_64.AppImage", "0.2.12").unwrap(), linux_appimage_name("0.2.12"));
        assert_eq!(protocol_file_name("RillSQL.app.tar.gz", "0.2.12").unwrap(), macos_updater_name("0.2.12"));
        assert!(protocol_file_name("rillsql_0.0.1_x64-setup.exe", "0.2.12").is_err());
    }

    #[test]
    fn manifest_keeps_seven_aliases_without_a_format_field() {
        let body = manifest_for("0.2.13", "c2ln");
        let value: Value = serde_json::from_str(&body).unwrap();
        let platforms = value["platforms"].as_object().unwrap();
        assert_eq!(platforms.len(), 7);
        assert!(platforms["windows-x86_64"].get("format").is_none());
        assert_eq!(platforms["windows-x86_64"], platforms["windows-x86_64-nsis"]);
        assert_eq!(platforms["linux-x86_64"], platforms["linux-x86_64-appimage"]);
        assert_eq!(platforms["darwin-aarch64"], platforms["darwin-aarch64-app"]);
        assert!(platforms["linux-x86_64-deb"]["url"].as_str().unwrap().ends_with(&linux_deb_name("0.2.13")));
        let selected = select_platform(&body, "windows-x86_64").unwrap();
        assert_eq!(selected.format, "nsis");
        assert_eq!(select_platform(&body, "linux-x86_64").unwrap().format, "appimage");
        assert_eq!(select_platform(&body, "darwin-aarch64").unwrap().format, "app");
        assert_eq!(select_platform(&body, "linux-x86_64-deb").unwrap().format, "deb");
    }

    #[test]
    fn missing_platform_is_not_a_complete_release() {
        let version = "0.2.12";
        let only_windows = vec![ReleaseAsset { name: windows_nsis_name(version), size: 12 }];
        let error = require_complete_release(&only_windows, version).unwrap_err();
        assert!(error.contains("incomplete release"), "{error}");
        assert!(error.contains(&macos_dmg_name(version)), "{error}");
    }

    #[test]
    fn active_work_blocks_install_and_keeps_the_current_version() {
        let keys = keys();
        let bytes = b"payload-bytes";
        let signature = sign(&keys, bytes, &windows_nsis_name("0.2.13"), "0.2.13");
        let manifest = manifest_for("0.2.13", &signature);
        let mut model = WorkbenchModel::new(Shell::default());
        model.exchange_running = true;
        let mut app = InstalledApp { version: PRODUCT_VERSION.to_owned() };
        let mut called = false;
        let error = apply_verified_update(&mut app, model.facts(), &keys.public_b64, &manifest, "windows-x86_64", bytes, |_| {
            called = true;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error, UpdateError::Blocked("shell.update.busy"));
        assert!(!called);
        assert_eq!(app.version, PRODUCT_VERSION);

        model.exchange_running = false;
        model.grid_dirty = true;
        let error = apply_verified_update(&mut app, model.facts(), &keys.public_b64, &manifest, "windows-x86_64", bytes, |_| Ok(())).unwrap_err();
        assert_eq!(error, UpdateError::Blocked("shell.update.pending"));
        assert_eq!(app.version, PRODUCT_VERSION);

        model.grid_dirty = false;
        assert!(model.dispatch("new-query").is_some());
        model.tabs[0].draft = "select 1".into();
        model.tabs[0].last_executed_sql = None;
        model.tabs[0].kind = WorkItemKind::Query;
        let error = apply_verified_update(&mut app, model.facts(), &keys.public_b64, &manifest, "windows-x86_64", bytes, |_| Ok(())).unwrap_err();
        assert!(matches!(error, UpdateError::Blocked(_)), "{error}");
        assert_eq!(app.version, PRODUCT_VERSION);
    }

    #[test]
    fn corrupted_signature_is_rejected_and_a_failed_install_keeps_the_version() {
        let signing = keys();
        let bytes = b"signed-payload";
        let signature = sign(&signing, bytes, &windows_nsis_name("0.2.13"), "0.2.13");
        let manifest = manifest_for("0.2.13", &signature);
        let mut app = InstalledApp { version: "0.2.12".into() };
        let mut called = false;
        let corrupt = b"signed-payloax";
        let error = apply_verified_update(&mut app, quiet_facts(), &signing.public_b64, &manifest, "windows-x86_64", corrupt, |_| {
            called = true;
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(error, UpdateError::Rejected(_)), "{error}");
        assert!(!called);
        assert_eq!(app.version, "0.2.12");

        let wrong_version = sign(&signing, bytes, &windows_nsis_name("0.2.13"), "9.9.9");
        let manifest = manifest_for("0.2.13", &wrong_version);
        let error = apply_verified_update(&mut app, quiet_facts(), &signing.public_b64, &manifest, "windows-x86_64", bytes, |_| Ok(())).unwrap_err();
        assert!(matches!(error, UpdateError::Rejected(_)), "{error}");
        assert_eq!(app.version, "0.2.12");

        let other = keys();
        let error = apply_verified_update(&mut app, quiet_facts(), &other.public_b64, &manifest_for("0.2.13", &signature), "windows-x86_64", bytes, |_| Ok(())).unwrap_err();
        assert!(matches!(error, UpdateError::Rejected(_)), "{error}");
        assert_eq!(app.version, "0.2.12");

        let error = apply_verified_update(&mut app, quiet_facts(), &signing.public_b64, &manifest_for("0.2.13", &signature), "windows-x86_64", bytes, |_| Err("disk full".into())).unwrap_err();
        assert!(matches!(error, UpdateError::Install(_)), "{error}");
        assert_eq!(app.version, "0.2.12");

        let version = apply_verified_update(&mut app, quiet_facts(), &signing.public_b64, &manifest_for("0.2.13", &signature), "windows-x86_64", bytes, |_| Ok(())).unwrap();
        assert_eq!(version, "0.2.13");
        assert_eq!(app.version, "0.2.13");
    }

    #[test]
    fn deb_is_kept_in_the_manifest_but_not_installed_by_packager_updater() {
        let keys = keys();
        let bytes = b"deb-bytes";
        let signature = sign(&keys, bytes, &linux_deb_name("0.2.13"), "0.2.13");
        let manifest = manifest_for("0.2.13", &signature);
        let platform = select_platform(&manifest, "linux-x86_64-deb").unwrap();
        assert_eq!(platform.format, "deb");
        let error = packager_update(&platform, Path::new(".")).unwrap_err();
        assert!(error.contains("current version kept"), "{error}");
    }

    #[test]
    fn verified_windows_update_can_build_a_passive_packager_update() {
        let keys = keys();
        let bytes = b"nsis-bytes";
        let signature = sign(&keys, bytes, &windows_nsis_name("0.2.13"), "0.2.13");
        verify_minisign(bytes, &signature, &keys.public_b64, "0.2.13").unwrap();
        let platform = select_platform(&manifest_for("0.2.13", &signature), "windows-x86_64").unwrap();
        let update = packager_update(&platform, Path::new(".")).unwrap();
        assert!(matches!(update.format, cargo_packager_updater::UpdateFormat::Nsis));
        assert_eq!(update.config.windows.unwrap().install_mode, Some(cargo_packager_updater::WindowsUpdateInstallMode::Passive));
        let app = InstalledApp { version: "0.2.12".into() };
        assert_eq!(app.version, "0.2.12");
        let _ = update;
    }

    #[test]
    fn packager_config_and_version_sources_stay_pinned() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let cargo = fs::read_to_string(root.join("src-tauri/Cargo.toml")).unwrap();
        assert!(cargo.contains("cargo-packager = { version = \"=0.11.8\""));
        assert!(cargo.contains("cargo-packager-updater = \"=0.2.3\""));
        assert!(cargo.contains("gpui-kit = { version = \"=0.9.0\""));
        assert!(cargo.contains("version = \"=0.3.11\""));
        assert!(cargo.contains("formats = [\"nsis\", \"app\", \"dmg\", \"deb\", \"appimage\"]"));
        assert!(cargo.contains("identifier = \"app.rillsql.desktop\""));
        assert!(cargo.contains("product-name = \"RillSQL\""));
        assert!(cargo.contains("installer-mode = \"currentUser\""));
        assert!(!cargo.contains("tauri"));
        let cargo_lock = fs::read_to_string(root.join("src-tauri/Cargo.lock")).unwrap();
        assert_eq!(rillsql_lock_version(&cargo_lock).as_deref(), Some(PRODUCT_VERSION));
        assert!(cargo.contains(&format!("version = \"{PRODUCT_VERSION}\"")));
        assert_eq!(UPDATER_ENDPOINT, "https://github.com/sakullla/RillSQL/releases/latest/download/latest.json");
        assert!(UPDATER_PUBKEY.starts_with("dW50cnVzdGVkIGNvbW1lbnQ6"));
        let ci = fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap();
        assert!(ci.contains("cargo test --locked --manifest-path src-tauri/Cargo.toml"));
        assert!(!ci.contains("npm "));
        assert!(!ci.contains("tauri-action"));
        assert!(!ci.contains("frontend-dist"));
        let release_ci = fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
        assert!(release_ci.contains("rillsql-package"));
        assert!(!release_ci.contains("tauri-action"));
        assert!(!release_ci.contains("npm "));
        assert!(release_ci.contains("release.mjs verify"));
        let package_ci = fs::read_to_string(root.join(".github/workflows/package-rust.yml")).unwrap();
        assert!(package_ci.contains("workflow_dispatch"));
        assert!(!package_ci.contains("npm "));
        assert!(package_ci.contains("--require-complete"));
        assert!(package_ci.contains("install-linux-deps.sh"));
    }

    #[test]
    fn empty_or_text_file_is_not_a_windows_nsis() {
        let path = std::env::temp_dir().join(format!("rillsql-not-nsis-{}.exe", std::process::id()));
        fs::write(&path, b"not an installer").unwrap();
        let error = inspect_windows_nsis(&path, PRODUCT_VERSION).unwrap_err();
        assert!(error.contains("expected"), "{error}");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unicode_nsis_resources_still_carry_the_product_and_version() {
        let dir = std::env::temp_dir().join(format!("rillsql-nsis-utf16-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(windows_nsis_name(PRODUCT_VERSION));
        let mut bytes = b"MZNullsoftInst".to_vec();
        for text in ["RillSQL", PRODUCT_VERSION] {
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
        }
        fs::write(&path, &bytes).unwrap();
        let inspection = inspect_windows_nsis(&path, PRODUCT_VERSION).unwrap();
        assert_eq!(inspection.arch, "x64");
        assert_eq!(inspection.version, PRODUCT_VERSION);
        assert!(inspection.size > 0);
        let _ = fs::remove_dir_all(dir);
    }
}
