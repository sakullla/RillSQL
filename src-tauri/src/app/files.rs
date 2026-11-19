use std::path::{Path, PathBuf};

pub fn accept_open_path(path: &Path) -> Result<PathBuf, String> {
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    if !canonical.is_file() {
        return Err(format!("not a file: {}", canonical.display()));
    }
    Ok(canonical)
}

pub fn accept_save_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let parent = absolute
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("save path has no parent: {}", absolute.display()))?;
    let parent = parent.canonicalize().map_err(|error| error.to_string())?;
    if !parent.is_dir() {
        return Err(format!("save parent is not a directory: {}", parent.display()));
    }
    let name = absolute
        .file_name()
        .ok_or_else(|| format!("save path has no file name: {}", absolute.display()))?;
    Ok(parent.join(name))
}

pub fn accept_drop_path(path: &Path) -> Result<PathBuf, String> {
    accept_open_path(path)
}

/// “打开文件”选中的一个文件该怎么打开（ADR-3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRoute {
    /// 读进查询编辑器。
    SqlText,
    /// 作为本地数据库文件建立连接。
    Database(crate::connections::model::Engine),
    /// 认不出：询问以 SQL 文本还是数据库文件打开。
    Ask,
}

/// 路由判定：文件头魔数优先于扩展名；`.sql` 进编辑器；文件型引擎的扩展名按数据库打开；其余询问。
pub fn route_open_path(path: &Path, header: &[u8]) -> OpenRoute {
    if let Some(engine) = crate::adapters::engine_for_file_header(header) {
        return OpenRoute::Database(engine);
    }
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default();
    if ext.eq_ignore_ascii_case("sql") {
        return OpenRoute::SqlText;
    }
    match crate::adapters::engine_for_file_extension(ext) {
        Some(engine) => OpenRoute::Database(engine),
        None => OpenRoute::Ask,
    }
}

/// 拖放时交给数据交换导入预览的文件：导入认识的表格格式（.csv/.json 直接预览，.xlsx/.xls 给出走导入对话框的提示）。
/// 其它认不出的文件与“打开文件”一样询问以 SQL 文本还是数据库文件打开。
pub fn is_drop_import_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "csv" | "json" | "xlsx" | "xls")
}

/// 读出判断文件头需要的开头字节；读不到（权限、空文件）时返回已读到的部分。
pub fn read_file_header(path: &Path) -> Vec<u8> {
    use std::io::Read;
    let wanted = crate::adapters::file_signature_len();
    let mut header = vec![0u8; wanted];
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let mut read = 0;
    while read < wanted {
        match file.read(&mut header[read..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    header.truncate(read);
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_save_and_drop_keep_real_absolute_paths() {
        let dir = std::env::temp_dir().join(format!("rillsql-gpui-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("查询.sql");
        std::fs::write(&file, "select '中文';\n").unwrap();
        let opened = accept_open_path(&file).unwrap();
        assert_eq!(opened, file.canonicalize().unwrap());
        assert!(opened.is_absolute());
        let dropped = accept_drop_path(&file).unwrap();
        assert_eq!(dropped, opened);
        let saved = accept_save_path(&dir.join("new.sql")).unwrap();
        assert_eq!(saved.file_name().unwrap(), "new.sql");
        assert!(saved.is_absolute());
        assert!(accept_drop_path(Path::new("missing-not-real.sql")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_file_routes_by_header_first_then_extension_and_asks_otherwise() {
        use crate::adapters::registry::LOCAL_FILE;
        let dir = std::env::temp_dir().join(format!("rillsql-open-route-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut database = b"SQLite format 3\0".to_vec();
        database.extend_from_slice(&[0x10, 0x00, 0x01, 0x01, 0x00, 0x40, 0x20, 0x20]);
        let cases: [(&str, &[u8], OpenRoute); 9] = [
            ("query.sql", b"select 1;\n", OpenRoute::SqlText),
            ("UPPER.SQL", b"select 1;\n", OpenRoute::SqlText),
            ("shop.db", &database, OpenRoute::Database(LOCAL_FILE)),
            // 文件头优先：扩展名不对、甚至写成 .sql 的数据库文件也按数据库打开。
            ("backup.bin", &database, OpenRoute::Database(LOCAL_FILE)),
            ("mislabeled.sql", &database, OpenRoute::Database(LOCAL_FILE)),
            // 新建的空数据库文件没有文件头：按扩展名。
            ("empty.sqlite3", b"", OpenRoute::Database(LOCAL_FILE)),
            ("notes.txt", b"select 1;\n", OpenRoute::Ask),
            ("no-extension", b"hello", OpenRoute::Ask),
            ("short", b"SQLite", OpenRoute::Ask),
        ];
        for (name, bytes, expected) in cases {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let header = read_file_header(&path);
            assert!(header.len() <= crate::adapters::file_signature_len());
            assert_eq!(route_open_path(&path, &header), expected, "{name}");
        }
        assert_eq!(read_file_header(&dir.join("missing.db")), Vec::<u8>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_tabular_formats_dropped_go_to_import_preview() {
        for name in ["rows.csv", "ROWS.CSV", "data.json", "book.xlsx", "old.xls"] {
            assert!(is_drop_import_path(Path::new(name)), "{name}");
        }
        for name in ["notes.txt", "no-extension", "query.sql", "shop.db", "dump.bak"] {
            assert!(!is_drop_import_path(Path::new(name)), "{name}");
        }
    }
}
