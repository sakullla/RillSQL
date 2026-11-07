fn main() {
    let dist = std::path::Path::new("../dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(dist).expect("create dist");
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>RillSQL</title></head><body><div id=\"app\"></div></body></html>",
        )
        .expect("write dist placeholder");
    }
    // GPUI's Win32 snapshot already embeds an application manifest. Tauri's
    // default manifest is a second RT_MANIFEST and the Windows linker rejects
    // the duplicate. Icons, version info and generate_context! still run.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("tauri build");
}
