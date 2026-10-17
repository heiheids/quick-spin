//! TOML 文件仓库：定位、读写、按 mtime 热重载。
//!
//! 用户配置（`config.toml`）与运行时状态（`state.toml`）共用这一套实现，
//! 两者差别只在文件位置和载入后的规整函数（`post_load`）。
//!
//! 热重载刻意采用"按需比对 mtime"，而不是挂 `ReadDirectoryChangesW` 监听：
//! 后者需要常驻一个线程加一个目录句柄；前者一次 `stat` 只要几微秒，
//! 由主循环低频调用即可。这符合"低内存、低开销"的目标，
//! 代价是最多延迟一秒生效 —— 改配色时完全无感。

// 仓库的完整能力要等主事件循环接入后才会全部用到。
// 事件循环落地后请删掉本行。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{AppError, Result};

/// 载入后不需要做任何规整时使用的空函数。
pub fn no_post_load<T>(_: &mut T) {}

/// 一个 TOML 文件仓库。
pub struct TomlStore<T> {
    path: PathBuf,
    value: T,
    /// 载入后规整：夹紧越界值、补派生字段。每次从磁盘读取后都会调用。
    /// 存成函数指针而不是泛型参数，是为了让 `TomlStore<T>` 只有一个类型参数。
    post_load: fn(&mut T),
    /// 上次读写时的文件修改时间，用于判断是否需要重载。
    last_loaded: Option<SystemTime>,
    /// 内存中的值是否已改动、待写回。
    dirty: bool,
}

impl<T> TomlStore<T>
where
    T: Serialize + DeserializeOwned + Default,
{
    /// 打开仓库。文件不存在时用默认值生成一份并落盘，方便用户直接编辑。
    pub fn open(path: PathBuf, post_load: fn(&mut T)) -> Result<Self> {
        if path.is_file() {
            let value = read(&path, post_load)?;
            let last_loaded = file_mtime(&path);
            Ok(Self {
                path,
                value,
                post_load,
                last_loaded,
                dirty: false,
            })
        } else {
            let mut value = T::default();
            post_load(&mut value);
            let mut store = Self {
                path,
                value,
                post_load,
                last_loaded: None,
                dirty: true,
            };
            store.save()?;
            Ok(store)
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    /// 可变访问即视为"已修改"，退出或定时落盘时统一写出。
    pub fn get_mut(&mut self) -> &mut T {
        self.dirty = true;
        &mut self.value
    }

    pub fn save(&mut self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| AppError::io(dir, e))?;
        }
        let text = toml::to_string_pretty(&self.value)?;
        std::fs::write(&self.path, text).map_err(|e| AppError::io(self.path.as_path(), e))?;
        self.last_loaded = file_mtime(&self.path);
        self.dirty = false;
        Ok(())
    }

    /// 只有改动过才写盘，避免无谓的磁盘写入。
    pub fn save_if_dirty(&mut self) -> Result<()> {
        if self.dirty { self.save() } else { Ok(()) }
    }

    /// 磁盘上的文件被外部改动时重新载入。返回是否真的发生了重载。
    pub fn reload_if_changed(&mut self) -> Result<bool> {
        let Some(mtime) = file_mtime(&self.path) else {
            return Ok(false);
        };
        if Some(mtime) == self.last_loaded {
            return Ok(false);
        }

        // 编辑器保存的瞬间可能读到写了一半的文件。
        // 解析失败就保留旧值，下一轮再试，绝不因为一次坏读而崩掉界面。
        match read(&self.path, self.post_load) {
            Ok(value) => {
                self.value = value;
                self.last_loaded = Some(mtime);
                self.dirty = false;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }
}

fn read<T>(path: &Path, post_load: fn(&mut T)) -> Result<T>
where
    T: DeserializeOwned,
{
    let text = std::fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;
    let mut value: T = toml::from_str(&text)?;
    post_load(&mut value);
    Ok(value)
}

/// 取文件修改时间；任何一步失败都返回 `None`（当作"无法判断"）。
fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}
