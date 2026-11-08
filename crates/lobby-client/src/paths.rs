//! Where the client keeps its files: logs under `<data_dir>/logs`, the
//! Lobbies in `<data_dir>/state.json`, the media cache under `<data_dir>/cache`.

use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::LobbyOrigin;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathsError {
    #[error("no per-user data directory is known for this system; set LOBBY_USER_DATA_DIR")]
    NoDataDir,
}

impl Paths {
    pub const APP_NAME: &'static str = "lobby-rs";

    /// `user_data_dir` (`LOBBY_USER_DATA_DIR`) wins. Otherwise the per-user data directory of
    /// `lobby-rs`, or `lobby-rs-<instance>` so two dev instances never share files.
    pub fn resolve(
        instance_id: Option<&str>,
        user_data_dir: Option<&Path>,
    ) -> Result<Paths, PathsError> {
        if let Some(dir) = user_data_dir {
            return Ok(Paths {
                data_dir: dir.to_owned(),
            });
        }
        let app = match instance_id {
            Some(id) => format!("{}-{id}", Self::APP_NAME),
            None => Self::APP_NAME.to_owned(),
        };
        let dirs = directories::ProjectDirs::from("", "", &app).ok_or(PathsError::NoDataDir)?;
        Ok(Paths {
            data_dir: dirs.data_dir().to_owned(),
        })
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    /// One Lobby's blobs, content-addressed:
    /// `<data_dir>/cache/media/<origin-hash>/<blob_id>[.preview]`. The origin is hashed so a
    /// host name never has to be a directory name.
    pub fn media_cache_dir(&self, origin: &LobbyOrigin) -> PathBuf {
        let digest = Sha256::digest(origin.as_str().as_bytes());
        let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        self.data_dir.join("cache").join("media").join(hash)
    }

    pub fn open_dir(&self) -> PathBuf {
        self.data_dir.join("cache").join("open")
    }
}
