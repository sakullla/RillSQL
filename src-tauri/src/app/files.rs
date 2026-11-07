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
}
