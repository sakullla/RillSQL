//! cargo-packager 0.11.8 的发布入口。
//!
//! 不在这个进程里再调 cargo：`cargo run` 会占着 target 目录锁。
//! 先单独构建 `rillsql`，再运行本命令。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rillsql_lib::release::{collect_release_assets, inspect_windows_nsis, require_complete_release, windows_nsis_name, PRODUCT_VERSION};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            if err.starts_with("incomplete release") { ExitCode::from(3) } else { ExitCode::from(1) }
        }
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse(env::args().skip(1))?;
    if let Some(directory) = &args.check_dir {
        return check_directory(directory, args.require_complete);
    }
    #[cfg(feature = "packager")]
    {
        package(&args)
    }
    #[cfg(not(feature = "packager"))]
    {
        let _ = args;
        Err("packaging requires: cargo run --manifest-path src-tauri/Cargo.toml --bin rillsql-package --features packager".into())
    }
}

struct Args {
    formats: Vec<String>,
    out: PathBuf,
    check_dir: Option<PathBuf>,
    require_complete: bool,
}

impl Args {
    fn parse(values: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Args {
            formats: vec!["nsis".into(), "app".into(), "dmg".into(), "deb".into(), "appimage".into()],
            out: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("packager"),
            check_dir: None,
            require_complete: false,
        };
        let values: Vec<String> = values.collect();
        let mut index = 0;
        while index < values.len() {
            let arg = &values[index];
            match arg.as_str() {
                "--require-complete" => args.require_complete = true,
                "--formats" | "--out" | "--check-dir" => {
                    let value = values.get(index + 1).ok_or_else(|| format!("{arg} needs a value"))?;
                    match arg.as_str() {
                        "--formats" => {
                            args.formats = value.split(',').filter(|item| !item.is_empty()).map(ToOwned::to_owned).collect();
                        }
                        "--out" => args.out = PathBuf::from(value),
                        "--check-dir" => args.check_dir = Some(PathBuf::from(value)),
                        _ => unreachable!(),
                    }
                    index += 1;
                }
                "--help" => {
                    println!("rillsql-package [--formats nsis,app,dmg,deb,appimage] [--out DIR] [--check-dir DIR] [--require-complete]");
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument {other}")),
            }
            index += 1;
        }
        if args.formats.is_empty() {
            return Err("at least one package format is required".into());
        }
        Ok(args)
    }
}

#[cfg(feature = "packager")]
fn packaged_binary() -> Result<PathBuf, String> {
    let exe = env::current_exe().map_err(|err| err.to_string())?;
    let dir = exe.parent().ok_or_else(|| format!("{} has no directory", exe.display()))?;
    let binary = dir.join(if cfg!(windows) { "rillsql.exe" } else { "rillsql" });
    if binary.is_file() {
        return Ok(binary);
    }
    Err(format!(
        "release binary missing beside {}: {}\nBuild it before packaging so cargo is not started while this process holds the target lock:\n  cargo build --release --locked --manifest-path src-tauri/Cargo.toml --bin rillsql",
        exe.display(),
        binary.display()
    ))
}

fn check_directory(directory: &Path, require_complete: bool) -> Result<(), String> {
    let assets = collect_release_assets(directory)?;
    if assets.is_empty() {
        return Err(format!("no package files in {}", directory.display()));
    }
    if let Some(nsis) = find_named(directory, &windows_nsis_name(PRODUCT_VERSION))? {
        let inspection = inspect_windows_nsis(&nsis, PRODUCT_VERSION)?;
        println!("windows-nsis {} bytes version {} arch {}", inspection.size, inspection.version, inspection.arch);
    }
    match require_complete_release(&assets, PRODUCT_VERSION) {
        Ok(()) => println!("complete release {}", PRODUCT_VERSION),
        Err(err) if !require_complete => println!("{err}"),
        Err(err) => return Err(err),
    }
    Ok(())
}

fn find_named(directory: &Path, name: &str) -> Result<Option<PathBuf>, String> {
    let mut found = None;
    visit(directory, &mut |path| {
        if path.file_name().and_then(|item| item.to_str()) == Some(name) {
            found = Some(path.to_path_buf());
        }
    })?;
    Ok(found)
}

fn visit(directory: &Path, visit_file: &mut dyn FnMut(&Path)) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|err| format!("read {}: {err}", directory.display()))? {
        let path = entry.map_err(|err| err.to_string())?.path();
        if path.is_dir() {
            visit(&path, visit_file)?;
        } else {
            visit_file(&path);
        }
    }
    Ok(())
}

#[cfg(feature = "packager")]
fn package(args: &Args) -> Result<(), String> {
    use std::io::Write;

    use cargo_packager::config::LogLevel;

    cargo_packager::init_tracing_subscriber(0);
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let binary = packaged_binary()?;
    let stage = manifest_dir.join("target").join("packager-stage");
    fs::create_dir_all(&stage).map_err(|err| err.to_string())?;
    let staged = stage.join(if cfg!(windows) { "RillSQL.exe" } else { "RillSQL" });
    fs::copy(&binary, &staged).map_err(|err| format!("stage binary: {err}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).map_err(|err| err.to_string())?;
    }

    let mut config = load_config(&manifest_dir)?;
    config.version = PRODUCT_VERSION.to_owned();
    config.binaries_dir = Some(stage);
    config.out_dir = args.out.clone();
    config.log_level = Some(LogLevel::Info);
    config.formats = Some(parse_formats(&args.formats)?);
    fs::create_dir_all(&config.out_dir).map_err(|err| err.to_string())?;
    env::set_current_dir(&manifest_dir).map_err(|err| err.to_string())?;

    let outputs = cargo_packager::package(&config).map_err(|err| format!("cargo-packager {}: {err}", rillsql_lib::release::PACKAGER_VERSION))?;
    normalize_outputs(&args.out, &outputs)?;
    sign_outputs(&args.out)?;
    check_directory(&args.out, args.require_complete)?;
    if let Some(nsis) = find_named(&args.out, &windows_nsis_name(PRODUCT_VERSION))? {
        let inspection = inspect_windows_nsis(&nsis, PRODUCT_VERSION)?;
        let report = args.out.join("windows-nsis.txt");
        let mut file = fs::File::create(&report).map_err(|err| err.to_string())?;
        writeln!(file, "name={}", inspection.name).map_err(|err| err.to_string())?;
        writeln!(file, "size={}", inspection.size).map_err(|err| err.to_string())?;
        writeln!(file, "version={}", inspection.version).map_err(|err| err.to_string())?;
        writeln!(file, "arch={}", inspection.arch).map_err(|err| err.to_string())?;
        writeln!(file, "path={}", nsis.display()).map_err(|err| err.to_string())?;
        println!("wrote {}", report.display());
    }
    Ok(())
}

#[cfg(feature = "packager")]
fn load_config(manifest_dir: &Path) -> Result<cargo_packager::Config, String> {
    #[derive(serde::Deserialize)]
    struct ManifestFile {
        package: PackageTable,
    }
    #[derive(serde::Deserialize)]
    struct PackageTable {
        metadata: MetadataTable,
    }
    #[derive(serde::Deserialize)]
    struct MetadataTable {
        packager: cargo_packager::Config,
    }
    let text = fs::read_to_string(manifest_dir.join("Cargo.toml")).map_err(|err| err.to_string())?;
    let manifest: ManifestFile = toml::from_str(&text).map_err(|err| format!("Packager metadata: {err}"))?;
    Ok(manifest.package.metadata.packager)
}

#[cfg(feature = "packager")]
fn parse_formats(names: &[String]) -> Result<Vec<cargo_packager::PackageFormat>, String> {
    names
        .iter()
        .map(|name| serde_json::from_value(serde_json::Value::String(name.clone())).map_err(|err| format!("unknown format {name}: {err}")))
        .collect()
}

#[cfg(feature = "packager")]
fn normalize_outputs(out: &Path, outputs: &[cargo_packager::PackageOutput]) -> Result<(), String> {
    for output in outputs {
        for path in &output.paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
            if path.is_dir() {
                #[cfg(unix)]
                if name.ends_with(".app") {
                    archive_app(path, &out.join(rillsql_lib::release::macos_updater_name(PRODUCT_VERSION)))?;
                }
                #[cfg(not(unix))]
                let _ = name;
                continue;
            }
            let protocol = rillsql_lib::release::protocol_file_name(name, PRODUCT_VERSION)?;
            let dest = out.join(protocol);
            if path != &dest {
                if dest.exists() {
                    fs::remove_file(&dest).map_err(|err| err.to_string())?;
                }
                fs::rename(path, &dest).map_err(|err| format!("rename {} -> {}: {err}", path.display(), dest.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(all(feature = "packager", unix))]
fn archive_app(app_dir: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        fs::remove_file(dest).map_err(|err| err.to_string())?;
    }
    let parent = app_dir.parent().ok_or_else(|| format!("{} has no parent", app_dir.display()))?;
    let name = app_dir.file_name().ok_or_else(|| format!("{} has no name", app_dir.display()))?;
    let status = std::process::Command::new("tar").arg("-czf").arg(dest).arg("-C").arg(parent).arg(name).status().map_err(|err| err.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("tar exited {status}")) }
}

#[cfg(feature = "packager")]
fn sign_outputs(out: &Path) -> Result<(), String> {
    let Some(private_key) = env::var("TAURI_SIGNING_PRIVATE_KEY").ok().or_else(|| env::var("CARGO_PACKAGER_SIGN_PRIVATE_KEY").ok()) else {
        println!("updater signing key is not set; signatures were not created");
        return Ok(());
    };
    let password = env::var("TAURI_SIGNING_PRIVATE_KEY_PASSWORD").ok().or_else(|| env::var("CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD").ok());
    let secret = decode_secret(&private_key, password.as_deref())?;
    let mut files = Vec::new();
    visit(out, &mut |path| {
        if path.is_file() {
            files.push(path.to_path_buf());
        }
    })?;
    let allowed = rillsql_lib::release::installer_names(PRODUCT_VERSION);
    let updater = rillsql_lib::release::macos_updater_name(PRODUCT_VERSION);
    for path in files {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        if allowed.iter().any(|item| item == name) || name == updater {
            write_signature(&secret, &path)?;
        }
    }
    Ok(())
}

#[cfg(feature = "packager")]
fn decode_secret(private_key: &str, password: Option<&str>) -> Result<minisign::SecretKey, String> {
    use base64::Engine;
    let decoded = if private_key.contains("untrusted comment:") {
        private_key.to_owned()
    } else {
        let bytes = base64::engine::general_purpose::STANDARD.decode(private_key.trim()).map_err(|err| format!("signing key: {err}"))?;
        String::from_utf8(bytes).map_err(|_| "signing key is not utf-8".to_owned())?
    };
    let secret = minisign::SecretKeyBox::from_string(&decoded).map_err(|err| format!("signing key: {err}"))?;
    secret.into_secret_key(password.map(str::to_owned)).map_err(|err| format!("signing key password: {err}"))
}

#[cfg(feature = "packager")]
fn write_signature(secret: &minisign::SecretKey, path: &Path) -> Result<(), String> {
    use std::io::Write;
    use base64::Engine;
    use std::time::{SystemTime, UNIX_EPOCH};

    let file = fs::File::open(path).map_err(|err| err.to_string())?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("package");
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|err| err.to_string())?.as_secs();
    let comment = format!("timestamp:{stamp}\tfile:{name}\tversion:{PRODUCT_VERSION}");
    let signature = minisign::sign(None, secret, file, Some(comment.as_str()), Some("signature from tauri secret key")).map_err(|err| format!("sign {}: {err}", path.display()))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(signature.to_string());
    let sig_path = PathBuf::from(format!("{}.sig", path.display()));
    let mut output = fs::File::create(&sig_path).map_err(|err| err.to_string())?;
    output.write_all(encoded.as_bytes()).map_err(|err| err.to_string())?;
    println!("signed {}", sig_path.display());
    Ok(())
}

#[cfg(all(test, feature = "packager"))]
mod tests {
    use super::load_config;
    use std::path::PathBuf;

    #[test]
    fn packager_metadata_matches_cargo_packager_0_11_8() {
        let config = load_config(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(config.product_name, "RillSQL");
        assert_eq!(config.identifier.as_deref(), Some("app.rillsql.desktop"));
        let formats = config.formats.expect("formats");
        let names: Vec<_> = formats.iter().map(|format| format.short_name()).collect();
        assert_eq!(names, ["nsis", "app", "dmg", "deb", "appimage"]);
    }
}

