fn main() {
    // Windows 可执行文件必须自带图标资源，桌面快捷方式和任务栏才不会退化成通用图标。
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        println!("cargo:rerun-if-changed=icons/app.rc");
        println!("cargo:rerun-if-changed=icons/icon.ico");
        let _ = embed_resource::compile("icons/app.rc", embed_resource::NONE);
    }
}
