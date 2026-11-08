//! The Lobby desktop app. GPUI lives only in this crate.

pub mod add_lobby;
pub mod appearance;
pub mod assets;
pub mod auth_view;
#[cfg(feature = "automation")]
pub mod automation;
pub mod clipboard;
pub mod components;
pub mod entry_entity;
pub mod files;
pub mod fonts;
#[cfg(feature = "gallery")]
pub mod gallery;
pub mod hand_over;
pub mod host;
pub mod host_env;
pub mod issues;
pub mod links;
pub mod lobbies;
pub mod lobby_icons;
pub mod lobby_view;
pub mod logging;
pub mod os_toasts;
pub mod runtime;
pub mod scale;
pub mod session_entity;
pub mod settings_view;
pub mod shell;
pub mod updates;
pub mod voice_settings;
pub mod window_bounds;

pub use host_env::HostEnv;
pub use runtime::TokioRuntime;
pub use session_entity::{SessionEntity, SessionUiEvent};
pub use shell::{Exit, QuitState, Route, RouteName, Shell, ShellDeps, SwitchTarget};
