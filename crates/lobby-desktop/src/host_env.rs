//! Host switches from the environment.

use std::{ffi::OsString, path::PathBuf};

/// What the person and the OS call the app: the window and tray name, and the Windows
/// AppUserModelID's prefix (GPUI registers it for toasts).
pub const APP_NAME: &str = "Lobby";
pub const AUMID_PREFIX: &str = "LobbyRs.Lobby";
#[cfg(target_os = "linux")]
pub use lobby_client::APP_ID;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEnv {
    pub instance_id: Option<String>,
    pub user_data_dir: Option<PathBuf>,
    /// `ALLOW_MULTIPLE_INSTANCES=1`: a second launch of the same profile starts its own app instead
    /// of showing the first.
    pub allow_multiple_instances: bool,
    pub fake_media: bool,
    pub insecure_tokens: bool,
    pub allow_http: bool,
    pub automation: bool,
    /// `LOBBY_AUTOMATION_VISIBLE=1`: a driven window opens on screen (still without taking focus)
    /// instead of off it.
    pub automation_visible: bool,
    /// `LOBBY_UPDATE_URL`: a release feed laid out like GitHub Releases, in place of the one a
    /// published build knows. For trying the updater against a local server.
    pub update_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostEnvError {
    #[error("LOBBY_INSTANCE_ID must be 1-32 characters of [A-Za-z0-9_-], got {0:?}")]
    InstanceId(String),
}

impl HostEnv {
    pub fn from_env() -> Result<HostEnv, HostEnvError> {
        Self::from_vars(std::env::vars_os())
    }

    pub fn from_vars(
        vars: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<HostEnv, HostEnvError> {
        let mut env = HostEnv::default();
        for (key, value) in vars {
            let value = value.to_string_lossy();
            match key.to_string_lossy().as_ref() {
                "LOBBY_INSTANCE_ID" if !value.is_empty() => {
                    let valid = value.len() <= 32
                        && value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
                    if !valid {
                        return Err(HostEnvError::InstanceId(value.into_owned()));
                    }
                    env.instance_id = Some(value.into_owned());
                }
                "LOBBY_USER_DATA_DIR" if !value.is_empty() => {
                    env.user_data_dir = Some(PathBuf::from(value.as_ref()));
                }
                "ALLOW_MULTIPLE_INSTANCES" => env.allow_multiple_instances = flag(&value),
                "LOBBY_FAKE_MEDIA" => env.fake_media = flag(&value),
                "LOBBY_INSECURE_TOKENS" => env.insecure_tokens = flag(&value),
                "LOBBY_ALLOW_HTTP" => env.allow_http = flag(&value),
                "LOBBY_AUTOMATION" => env.automation = flag(&value),
                "LOBBY_AUTOMATION_VISIBLE" => env.automation_visible = flag(&value),
                "LOBBY_UPDATE_URL" if !value.is_empty() => {
                    env.update_url = Some(value.into_owned());
                }
                _ => {}
            }
        }
        Ok(env)
    }

    pub fn aumid(&self) -> String {
        match &self.instance_id {
            Some(id) => format!("{AUMID_PREFIX}.{id}"),
            None => AUMID_PREFIX.into(),
        }
    }

    /// The tray icon's tooltip. A second instance (a driven client) says which it is.
    pub fn tray_tooltip(&self) -> String {
        match &self.instance_id {
            Some(id) => format!("{APP_NAME} ({id})"),
            None => APP_NAME.into(),
        }
    }
}

fn flag(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}
