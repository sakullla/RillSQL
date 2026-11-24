//! 表格列布局记忆（R3）：按“连接 + 库 + schema + 表”保存列顺序、隐藏、冻结与用户调过的列宽。
//! 单独存放在 `column-layouts.json`，与 `app-state.json` 同目录；只含本机界面偏好，不含任何行数据或密码。
//! 写盘先改内存再在后台原子写入；多次连续修改只保证最后一份落盘。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::atomic::write_atomic;

pub const COLUMN_LAYOUTS_FILE: &str = "column-layouts.json";
const COLUMN_LAYOUTS_VERSION: u32 = 1;
/// 最多记住多少张表；超出时丢掉最久没改过的布局。
pub const MAX_COLUMN_LAYOUTS: usize = 500;

/// 一张表的列布局。全部按列名记录：表结构变化后，不存在的列被忽略，新增的列按默认显示并排在末尾。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnLayout {
    /// 自定义的列显示顺序；空表示结构原始顺序。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    /// 隐藏的列。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// 冻结到哪一列（它与左侧的可见列固定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen: Option<String>,
    /// 用户拖过或自适应过的列宽；没调过的列每次按内容估宽。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub widths: BTreeMap<String, f32>,
}

impl ColumnLayout {
    pub fn is_default(&self) -> bool {
        self.order.is_empty() && self.hidden.is_empty() && self.frozen.is_none() && self.widths.is_empty()
    }

    /// 按当前表的列对齐：丢掉已不存在的列；顺序里没有的新列追加到末尾并保持显示；
    /// 万一全部列都被隐藏就全部显示（至少保留一列可见）。
    pub fn aligned(&self, columns: &[String]) -> ColumnLayout {
        let present: BTreeSet<&str> = columns.iter().map(String::as_str).collect();
        let mut seen = BTreeSet::new();
        let mut order: Vec<String> = self.order.iter().filter(|name| present.contains(name.as_str()) && seen.insert(name.as_str())).cloned().collect();
        if !order.is_empty() {
            for name in columns {
                if !seen.contains(name.as_str()) {
                    order.push(name.clone());
                }
            }
            if order == columns {
                order.clear();
            }
        }
        let mut hidden: Vec<String> = Vec::new();
        for name in &self.hidden {
            if present.contains(name.as_str()) && !hidden.contains(name) {
                hidden.push(name.clone());
            }
        }
        if !columns.is_empty() && hidden.len() >= columns.len() {
            hidden.clear();
        }
        let frozen = self.frozen.clone().filter(|name| present.contains(name.as_str()) && !hidden.contains(name));
        let widths = self.widths.iter().filter(|(name, width)| present.contains(name.as_str()) && width.is_finite()).map(|(name, width)| (name.clone(), *width)).collect();
        ColumnLayout { order, hidden, frozen, widths }
    }
}

/// 布局归属：同一连接的同名表在不同库或 schema 下各记一份。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnLayoutKey {
    pub connection_id: String,
    pub database: String,
    #[serde(default)]
    pub schema: String,
    pub table: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEntry {
    #[serde(flatten)]
    key: ColumnLayoutKey,
    layout: ColumnLayout,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredFile {
    version: u32,
    #[serde(default)]
    layouts: Vec<StoredEntry>,
}

#[derive(Debug, Default)]
struct Inner {
    loaded: bool,
    /// 按最近修改排序：末尾是最新的。
    entries: Vec<(ColumnLayoutKey, ColumnLayout)>,
    revision: u64,
}

/// 进程内共享的列布局表。`StateStore` 克隆时共用同一份。
#[derive(Debug)]
pub struct ColumnLayoutStore {
    path: PathBuf,
    inner: Mutex<Inner>,
    /// 串行化写盘：后到的旧代次直接跳过，不会盖掉更新的快照。
    write_gate: Mutex<()>,
}

impl ColumnLayoutStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self { path: data_dir.as_ref().join(COLUMN_LAYOUTS_FILE), inner: Mutex::new(Inner::default()), write_gate: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 读文件填充内存（只读一次）。文件不存在、损坏或版本不认识时按空布局处理，原文件不动——
    /// 下一次修改布局时才会被新内容替换。
    pub fn ensure_loaded(&self) {
        if self.lock().loaded {
            return;
        }
        let entries = read_entries(&self.path);
        let mut inner = self.lock();
        if !inner.loaded {
            inner.loaded = true;
            inner.entries = entries;
        }
    }

    /// 启动时在后台线程预读，界面第一次打开表时不必读盘。
    pub async fn preload(self: &Arc<Self>) {
        let store = Arc::clone(self);
        let _ = tokio::task::spawn_blocking(move || store.ensure_loaded()).await;
    }

    pub fn get(&self, key: &ColumnLayoutKey) -> Option<ColumnLayout> {
        self.ensure_loaded();
        self.lock().entries.iter().find(|(item, _)| item == key).map(|(_, layout)| layout.clone())
    }

    /// 记住了多少张表的布局。
    pub fn count(&self) -> usize {
        self.ensure_loaded();
        self.lock().entries.len()
    }

    /// 改内存里的布局；默认布局等于删除记录。返回要写盘的代次与内容；没有变化时为 None。
    pub fn put(&self, key: ColumnLayoutKey, layout: ColumnLayout) -> Option<(u64, Vec<u8>)> {
        self.ensure_loaded();
        let mut inner = self.lock();
        let existing = inner.entries.iter().position(|(item, _)| *item == key);
        match (existing, layout.is_default()) {
            (None, true) => return None,
            (Some(index), _) if inner.entries[index].1 == layout => return None,
            (Some(index), true) => {
                inner.entries.remove(index);
            }
            (Some(index), false) => {
                inner.entries.remove(index);
                inner.entries.push((key, layout));
            }
            (None, false) => inner.entries.push((key, layout)),
        }
        let overflow = inner.entries.len().saturating_sub(MAX_COLUMN_LAYOUTS);
        if overflow > 0 {
            inner.entries.drain(..overflow);
        }
        inner.revision += 1;
        let file = StoredFile {
            version: COLUMN_LAYOUTS_VERSION,
            layouts: inner.entries.iter().map(|(key, layout)| StoredEntry { key: key.clone(), layout: layout.clone() }).collect(),
        };
        let mut bytes = serde_json::to_vec_pretty(&file).ok()?;
        bytes.push(b'\n');
        Some((inner.revision, bytes))
    }

    /// 把 put 得到的快照写盘（阻塞调用，放在后台线程）。已有更新的代次时跳过，由更新的那次写。
    pub fn write_revision(&self, revision: u64, bytes: &[u8]) -> Result<(), String> {
        let _gate = self.write_gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.lock().revision != revision {
            return Ok(());
        }
        write_atomic(&self.path, bytes)
    }

    /// 修改并立即写盘（测试与同步调用方使用）。
    pub fn put_and_write(&self, key: ColumnLayoutKey, layout: ColumnLayout) -> Result<(), String> {
        match self.put(key, layout) {
            Some((revision, bytes)) => self.write_revision(revision, &bytes),
            None => Ok(()),
        }
    }
}

fn read_entries(path: &Path) -> Vec<(ColumnLayoutKey, ColumnLayout)> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_slice::<StoredFile>(&bytes) else {
        return Vec::new();
    };
    if file.version != COLUMN_LAYOUTS_VERSION {
        return Vec::new();
    }
    let mut entries: Vec<(ColumnLayoutKey, ColumnLayout)> = Vec::new();
    for entry in file.layouts {
        entries.retain(|(key, _)| *key != entry.key);
        entries.push((entry.key, entry.layout));
    }
    let overflow = entries.len().saturating_sub(MAX_COLUMN_LAYOUTS);
    entries.drain(..overflow);
    entries
}
