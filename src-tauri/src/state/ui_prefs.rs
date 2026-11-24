//! 界面小偏好（R11）：例如执行计划上次选的视图（画布 / 列表 / 原始）。
//! 单独存放在 `ui-prefs.json`，与 `app-state.json` 同目录；只含本机界面选择，不含任何数据。
//! 写盘先改内存再在后台原子写入；多次连续修改只保证最后一份落盘。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::atomic::write_atomic;

pub const UI_PREFS_FILE: &str = "ui-prefs.json";
const UI_PREFS_VERSION: u32 = 1;

/// 记住的界面选择。没有记录的项为 None，界面按自己的默认值处理。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiPrefs {
    /// 执行计划视图：`canvas` / `list` / `raw`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_view: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredFile {
    version: u32,
    #[serde(flatten)]
    prefs: UiPrefs,
}

#[derive(Debug, Default)]
struct Inner {
    loaded: bool,
    prefs: UiPrefs,
    revision: u64,
}

/// 进程内共享的界面偏好。`StateStore` 克隆时共用同一份。
#[derive(Debug)]
pub struct UiPrefsStore {
    path: PathBuf,
    inner: Mutex<Inner>,
    write_gate: Mutex<()>,
}

impl UiPrefsStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self { path: data_dir.as_ref().join(UI_PREFS_FILE), inner: Mutex::new(Inner::default()), write_gate: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 读文件填充内存（只读一次）。文件不存在、损坏或版本不认识时按默认处理，原文件不动。
    fn ensure_loaded(&self) {
        if self.lock().loaded {
            return;
        }
        let prefs = read_prefs(&self.path);
        let mut inner = self.lock();
        if !inner.loaded {
            inner.loaded = true;
            inner.prefs = prefs;
        }
    }

    pub fn get(&self) -> UiPrefs {
        self.ensure_loaded();
        self.lock().prefs.clone()
    }

    /// 改内存里的偏好。返回要写盘的代次与内容；没有变化时为 None。
    pub fn update(&self, change: impl FnOnce(&mut UiPrefs)) -> Option<(u64, Vec<u8>)> {
        self.ensure_loaded();
        let mut inner = self.lock();
        let mut next = inner.prefs.clone();
        change(&mut next);
        if next == inner.prefs {
            return None;
        }
        inner.prefs = next;
        inner.revision += 1;
        let file = StoredFile { version: UI_PREFS_VERSION, prefs: inner.prefs.clone() };
        let mut bytes = serde_json::to_vec_pretty(&file).ok()?;
        bytes.push(b'\n');
        Some((inner.revision, bytes))
    }

    /// 把 update 得到的快照写盘（阻塞调用，放在后台线程）。已有更新的代次时跳过，由更新的那次写。
    pub fn write_revision(&self, revision: u64, bytes: &[u8]) -> Result<(), String> {
        let _gate = self.write_gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.lock().revision != revision {
            return Ok(());
        }
        write_atomic(&self.path, bytes)
    }

    /// 修改并立即写盘（测试与同步调用方使用）。
    pub fn update_and_write(&self, change: impl FnOnce(&mut UiPrefs)) -> Result<(), String> {
        match self.update(change) {
            Some((revision, bytes)) => self.write_revision(revision, &bytes),
            None => Ok(()),
        }
    }
}

fn read_prefs(path: &Path) -> UiPrefs {
    let Ok(bytes) = std::fs::read(path) else {
        return UiPrefs::default();
    };
    match serde_json::from_slice::<StoredFile>(&bytes) {
        Ok(file) if file.version == UI_PREFS_VERSION => file.prefs,
        _ => UiPrefs::default(),
    }
}
