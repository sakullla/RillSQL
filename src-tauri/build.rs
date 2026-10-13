fn main() {
    let dist = std::path::Path::new("../dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(dist).expect("create dist");
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>澄库 · RillSQL</title></head><body><div id=\"app\"></div></body></html>",
        )
        .expect("write dist placeholder");
    }
    tauri_build::build()
}
