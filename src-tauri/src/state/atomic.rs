//! 同目录临时文件、flush、再 rename。失败时不替换目标文件。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let staged = stage(path, bytes)?;
    staged.commit()
}

pub(crate) struct Staged {
    tmp: PathBuf,
    dest: PathBuf,
}

pub(crate) fn stage(path: &Path, bytes: &[u8]) -> Result<Staged, String> {
    let parent = path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| format!("建不了状态目录：{err}"))?;
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or(super::document::APP_STATE_FILE);
    let tmp = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let write_result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(format!("写入状态临时文件失败：{err}。原状态未替换。"));
    }
    Ok(Staged { tmp, dest: path.to_path_buf() })
}

#[allow(dead_code)]
impl Staged {
    pub(crate) fn commit(self) -> Result<(), String> {
        match fs::rename(&self.tmp, &self.dest) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = fs::remove_file(&self.tmp);
                Err(format!("替换状态文件失败：{err}。原状态未替换。"))
            }
        }
    }

    pub(crate) fn abort(self) -> Result<(), String> {
        let _ = fs::remove_file(&self.tmp);
        Err("状态写入在替换前中止，原文件未改动。".to_owned())
    }
}

pub async fn write_atomic_off_ui(path: PathBuf, bytes: Vec<u8>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || write_atomic(&path, &bytes))
        .await
        .map_err(|err| format!("状态写入任务失败：{err}"))?
}

#[cfg(test)]
pub async fn write_atomic_thread(path: PathBuf, bytes: Vec<u8>) -> Result<std::thread::ThreadId, String> {
    tokio::task::spawn_blocking(move || {
        let id = std::thread::current().id();
        write_atomic(&path, &bytes)?;
        Ok(id)
    })
    .await
    .map_err(|err| format!("状态写入任务失败：{err}"))?
}
