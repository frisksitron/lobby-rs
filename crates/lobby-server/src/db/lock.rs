//! One server per database: a server holds an exclusive OS lock on `<database.path>.lock` until
//! its writer connection has closed, so a second writer can never break the single-writer
//! ordering, even after a shutdown that gave up waiting for a write.

use std::{
    fs::{File, TryLockError},
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another lobby-server process holds {}", .path.display())]
    Held { path: PathBuf },
    #[error("cannot lock {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct DbLock {
    _file: File,
}

impl DbLock {
    pub fn path_for(db_path: &Path) -> PathBuf {
        let mut name = db_path.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }

    pub fn acquire(db_path: &Path) -> Result<DbLock, LockError> {
        let path = Self::path_for(db_path);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|source| LockError::Io {
                path: path.clone(),
                source,
            })?;
        }
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| LockError::Io {
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(DbLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(LockError::Held { path }),
            Err(TryLockError::Error(source)) => Err(LockError::Io { path, source }),
        }
    }
}
