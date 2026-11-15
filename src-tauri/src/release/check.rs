//! 应用内检查更新：拉取发布页的 `latest.json`、与当前版本比较、下载并双重验签，再交给安装器。
//!
//! 网络读取都是阻塞调用，调用方放在独立线程里跑（不在 tokio 运行时线程上，也不在界面线程上）。
//! 任何一步失败都只返回原因：当前版本保持可用，不留下半装状态。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{packager_update, parse_manifest, select_platform, verify_minisign, version_is_newer, PlatformUpdate, PRODUCT_VERSION, UPDATER_ENDPOINT, UPDATER_PUBKEY};

/// 发布页：没有可自动安装的包时，用户从这里手动下载。
pub const RELEASES_PAGE: &str = "https://github.com/sakullla/RillSQL/releases";

/// 读取清单的超时：自动检查在后台静默进行，网络不好时不长时间挂着。
pub const MANIFEST_TIMEOUT: Duration = Duration::from_secs(15);

/// `latest.json` 里本机对应的平台键。与发布流程生成的键一致（不是 cargo-packager 的 `macos-*` 写法）。
pub fn manifest_target() -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("darwin-aarch64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else {
        None
    }
}

/// 这份安装能不能在应用内原地更新。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallRoute {
    /// 可以原地安装；路径是安装器要替换的对象（AppImage 文件、.app 包；Windows 上仅作占位）。
    InPlace(PathBuf),
    /// 只能去发布页手动下载：deb 等系统包、开发构建、无法定位安装位置。
    Manual,
}

/// 按本机安装形态判断更新路线。开发构建一律走手动，避免把正式安装包装到开发目录旁边。
pub fn install_route() -> InstallRoute {
    if cfg!(debug_assertions) {
        return InstallRoute::Manual;
    }
    let Ok(exe) = std::env::current_exe() else { return InstallRoute::Manual };
    route_for(std::env::consts::OS, &exe, std::env::var_os("APPIMAGE").map(PathBuf::from))
}

fn route_for(os: &str, exe: &Path, appimage: Option<PathBuf>) -> InstallRoute {
    match os {
        "windows" => InstallRoute::InPlace(exe.to_path_buf()),
        // RillSQL.app/Contents/MacOS/rillsql → 替换整个 .app 包。
        "macos" => exe
            .ancestors()
            .nth(3)
            .filter(|bundle| bundle.extension().is_some_and(|ext| ext == "app"))
            .map(|bundle| InstallRoute::InPlace(bundle.to_path_buf()))
            .unwrap_or(InstallRoute::Manual),
        // 只有以 AppImage 运行时才能原地替换；deb 安装由系统包管理器负责。
        "linux" => appimage
            .filter(|path| path.is_absolute() && path.is_file())
            .map(InstallRoute::InPlace)
            .unwrap_or(InstallRoute::Manual),
        _ => InstallRoute::Manual,
    }
}

/// 一次检查发现的新版本。`platform` 为空表示清单里没有本机可自动安装的包，只能去发布页。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateOffer {
    pub version: String,
    pub notes: String,
    pub pub_date: Option<String>,
    pub platform: Option<PlatformUpdate>,
    pub page_url: String,
}

impl UpdateOffer {
    /// 能否在应用内一键安装：既要清单里有本机的包，也要这份安装支持原地替换。
    pub fn installable(&self, route: &InstallRoute) -> bool {
        self.platform.is_some() && matches!(route, InstallRoute::InPlace(_))
    }
}

/// 比较清单与当前版本：不比当前新就是 `None`（包括清单版本更旧的情况）。
pub fn evaluate(manifest_json: &str, current: &str, target: Option<&str>) -> Result<Option<UpdateOffer>, String> {
    let manifest = parse_manifest(manifest_json)?;
    if !version_is_newer(&manifest.version, current)? {
        return Ok(None);
    }
    // deb 不能由应用自己安装；缺失或不完整的平台条目同样退回发布页，而不是让检查失败。
    let platform = target.and_then(|target| select_platform(manifest_json, target).ok()).filter(|platform| platform.format != "deb");
    Ok(Some(UpdateOffer {
        page_url: format!("{RELEASES_PAGE}/tag/v{}", manifest.version),
        version: manifest.version,
        notes: manifest.notes.trim().to_owned(),
        pub_date: manifest.pub_date,
        platform,
    }))
}

/// 清单地址。调试构建可用 `RILLSQL_DEBUG_UPDATE_ENDPOINT` 指向本地清单做验证；正式构建固定为发布页。
pub fn endpoint() -> String {
    #[cfg(debug_assertions)]
    if let Some(url) = std::env::var("RILLSQL_DEBUG_UPDATE_ENDPOINT").ok().filter(|url| !url.trim().is_empty()) {
        return url;
    }
    UPDATER_ENDPOINT.to_owned()
}

/// 阻塞读取清单文本。不要在 tokio 运行时线程上调用。
pub fn fetch_manifest(url: &str, timeout: Duration) -> Result<String, String> {
    use cargo_packager_updater::reqwest;
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(format!("RillSQL/{PRODUCT_VERSION}"))
        .build()
        .map_err(|err| err.to_string())?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(|err| network_text(&err))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    response.text().map_err(|err| network_text(&err))
}

fn network_text(err: &cargo_packager_updater::reqwest::Error) -> String {
    if err.is_timeout() {
        "timeout".to_owned()
    } else if err.is_connect() {
        "connection failed".to_owned()
    } else {
        err.to_string()
    }
}

/// 读取清单并与当前版本比较。
pub fn check_now(current: &str) -> Result<Option<UpdateOffer>, String> {
    let manifest = fetch_manifest(&endpoint(), MANIFEST_TIMEOUT)?;
    evaluate(&manifest, current, manifest_target())
}

/// 下载好、验过签、可以安装的包。
pub struct VerifiedPackage {
    pub update: cargo_packager_updater::Update,
    pub bytes: Vec<u8>,
}

/// 下载安装包并双重验签：先由安装器按内置公钥验签，再核对签名绑定的版本与清单版本一致。
/// `progress` 收到（已下载字节, 总字节）。
pub fn download_verified(platform: &PlatformUpdate, extract_path: &Path, progress: impl Fn(u64, Option<u64>)) -> Result<VerifiedPackage, String> {
    let update = packager_update(platform, extract_path)?;
    let done = AtomicU64::new(0);
    let bytes = update
        .download_extended(
            |chunk, total| {
                let now = done.fetch_add(chunk as u64, Ordering::Relaxed) + chunk as u64;
                progress(now, total);
            },
            || {},
        )
        .map_err(|err| err.to_string())?;
    verify_minisign(&bytes, &platform.signature, UPDATER_PUBKEY, &platform.version)?;
    Ok(VerifiedPackage { update, bytes })
}

/// 安装完成后重新打开新版本的命令（macOS 与 AppImage）。Windows 安装器自己负责重启。
pub fn relaunch_command(route: &InstallRoute) -> Option<std::process::Command> {
    let InstallRoute::InPlace(path) = route else { return None };
    if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg("-n").arg(path);
        Some(command)
    } else if cfg!(target_os = "linux") {
        Some(std::process::Command::new(path))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(version: &str, platforms: serde_json::Value) -> String {
        json!({ "version": version, "notes": "  修复若干问题\n", "pub_date": "2026-11-14T00:00:00Z", "platforms": platforms }).to_string()
    }

    fn entry(url: &str) -> serde_json::Value {
        json!({ "signature": "c2ln", "url": url })
    }

    #[test]
    fn newer_release_becomes_an_offer_with_notes_and_page() {
        let text = manifest("0.3.0", json!({ "linux-x86_64": entry("https://example.test/RillSQL_0.3.0_amd64.AppImage") }));
        let offer = evaluate(&text, "0.2.28", Some("linux-x86_64")).unwrap().expect("0.3.0 比 0.2.28 新");
        assert_eq!(offer.version, "0.3.0");
        assert_eq!(offer.notes, "修复若干问题");
        assert_eq!(offer.pub_date.as_deref(), Some("2026-11-14T00:00:00Z"));
        assert_eq!(offer.page_url, "https://github.com/sakullla/RillSQL/releases/tag/v0.3.0");
        let platform = offer.platform.clone().expect("清单里有本机的包");
        assert_eq!(platform.format, "appimage");
        assert!(offer.installable(&InstallRoute::InPlace(PathBuf::from("/opt/RillSQL.AppImage"))));
        assert!(!offer.installable(&InstallRoute::Manual), "deb/开发构建只能去发布页");
    }

    #[test]
    fn same_or_older_release_is_not_an_offer() {
        let platforms = json!({ "linux-x86_64": entry("https://example.test/a.AppImage") });
        assert_eq!(evaluate(&manifest("0.2.28", platforms.clone()), "0.2.28", Some("linux-x86_64")).unwrap(), None);
        assert_eq!(evaluate(&manifest("v0.2.27", platforms), "0.2.28", Some("linux-x86_64")).unwrap(), None, "清单落后于本机时不提示降级");
    }

    #[test]
    fn missing_or_deb_platform_falls_back_to_the_release_page() {
        let only_windows = manifest("0.3.0", json!({ "windows-x86_64": entry("https://example.test/RillSQL_0.3.0_x64-setup.exe") }));
        let offer = evaluate(&only_windows, "0.2.28", Some("darwin-aarch64")).unwrap().unwrap();
        assert!(offer.platform.is_none(), "缺本机平台：仍然提示新版本，但只能手动下载");
        let deb = manifest("0.3.0", json!({ "linux-x86_64": json!({ "signature": "c2ln", "url": "https://example.test/RillSQL_0.3.0_amd64.deb", "format": "deb" }) }));
        assert!(evaluate(&deb, "0.2.28", Some("linux-x86_64")).unwrap().unwrap().platform.is_none());
        let unsigned = manifest("0.3.0", json!({ "linux-x86_64": json!({ "signature": " ", "url": "https://example.test/a.AppImage" }) }));
        assert!(evaluate(&unsigned, "0.2.28", Some("linux-x86_64")).unwrap().unwrap().platform.is_none(), "没有签名的条目不能自动安装");
    }

    #[test]
    fn broken_manifests_report_a_reason() {
        assert!(evaluate("<html>404</html>", "0.2.28", None).is_err());
        assert!(evaluate(&json!({ "version": "", "platforms": {} }).to_string(), "0.2.28", None).is_err());
        assert!(evaluate(&json!({ "version": "next", "platforms": {} }).to_string(), "0.2.28", None).is_err());
    }

    /// 与发布流程实际生成的清单键一致，不是 cargo-packager 自带的 `macos-*` 写法。
    #[test]
    fn manifest_targets_match_the_release_workflow() {
        let target = manifest_target();
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(target, Some("linux-x86_64"));
        }
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(target, Some("darwin-aarch64"));
        }
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            assert_eq!(target, Some("windows-x86_64"));
        }
        let names: Vec<&str> = super::super::updater_names("1.0.0").iter().map(|(target, _)| *target).collect();
        if let Some(target) = target {
            assert!(names.contains(&target), "{target} 不在发布清单的平台键里：{names:?}");
        }
    }

    #[test]
    fn install_routes_follow_the_install_shape() {
        let app = Path::new("/Applications/RillSQL.app/Contents/MacOS/rillsql");
        assert_eq!(route_for("macos", app, None), InstallRoute::InPlace(PathBuf::from("/Applications/RillSQL.app")));
        assert_eq!(route_for("macos", Path::new("/usr/local/bin/rillsql"), None), InstallRoute::Manual);
        assert_eq!(route_for("linux", Path::new("/usr/bin/rillsql"), None), InstallRoute::Manual, "deb 安装交给包管理器");
        assert_eq!(route_for("linux", Path::new("/tmp/x"), Some(PathBuf::from("relative.AppImage"))), InstallRoute::Manual);
        let image = std::env::temp_dir().join(format!("rillsql-route-{}.AppImage", uuid::Uuid::new_v4()));
        std::fs::write(&image, b"x").unwrap();
        assert_eq!(route_for("linux", Path::new("/tmp/x"), Some(image.clone())), InstallRoute::InPlace(image.clone()));
        let _ = std::fs::remove_file(&image);
        assert!(matches!(route_for("windows", Path::new("C:/Program Files/RillSQL/rillsql.exe"), None), InstallRoute::InPlace(_)));
    }

    /// 真正走一次 HTTP：本地起一个只回一份清单的服务，读取、状态码与超时都按实际网络路径处理。
    #[test]
    fn fetch_reads_the_manifest_and_reports_http_errors() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = manifest("9.0.0", json!({}));
        let served = body.clone();
        let server = std::thread::spawn(move || {
            for (index, stream) in listener.incoming().take(2).enumerate() {
                let mut stream = stream.unwrap();
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer);
                let reply = if index == 0 {
                    format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{served}", served.len())
                } else {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
                };
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        let url = format!("http://{address}/latest.json");
        assert_eq!(fetch_manifest(&url, Duration::from_secs(5)).unwrap(), body);
        assert_eq!(fetch_manifest(&url, Duration::from_secs(5)).unwrap_err(), "HTTP 404");
        server.join().unwrap();
    }
}
