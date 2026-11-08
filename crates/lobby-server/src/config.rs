//! Configuration: defaults → TOML file → `LOBBY_*` env (nested with `__`). Unknown keys are
//! rejected, a bad value is a startup error, and 0 means 0. `validate` collects every problem
//! before failing; `main` then exits with 78 (EX_CONFIG).

use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use ipnet::IpNet;
use lobby_protocol::{Email, RestartPolicy, secret::SmtpPassword};
use serde::{Deserialize, Serialize};

use crate::mail::{TlsPolicy, is_this_machine, tls_policy};

/// `LOBBY_*` variables that are not config keys: the server's log format, and the desktop
/// client's host switches, which a developer may have set in the same shell.
const NON_CONFIG_ENV: &[&str] = &[
    "log_format",
    "instance_id",
    "user_data_dir",
    "fake_media",
    "insecure_tokens",
    "allow_http",
    "overlay_fault",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Total graceful-shutdown budget; keep it below the container stop grace period.
    #[serde(with = "humantime_serde")]
    pub shutdown_timeout: Duration,
    pub lobby: LobbyConfig,
    pub http: HttpConfig,
    pub ws: WsConfig,
    pub database: DatabaseConfig,
    pub storage: StorageConfig,
    pub auth: AuthConfig,
    pub smtp: SmtpConfig,
    pub sfu: SfuConfig,
    pub voice: VoiceConfig,
    pub jobs: JobsConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LobbyConfig {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    pub bind: SocketAddr,
    pub trusted_proxy_cidrs: Vec<IpNet>,
    /// The per-IP REST limits and the per-Member send throttle. Off only for tests and closed
    /// networks.
    pub rate_limits: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WsConfig {
    #[serde(with = "humantime_serde")]
    pub identify_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub ping_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub pong_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub write_timeout: Duration,
    /// Frames queued per connection before it is kicked as a slow consumer (4008).
    pub outbound_queue: usize,
    pub max_frame_bytes: usize,
    /// 0 = unlimited.
    pub max_unidentified_per_ip: usize,
    /// 0 = unlimited.
    pub max_unidentified_global: usize,
    /// A Member's sends with `http.rate_limits` on: one per this, sustained, after a burst of
    /// [`crate::lobby::SEND_BURST`].
    #[serde(with = "humantime_serde")]
    pub send_every: Duration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub blob_root: PathBuf,
    pub upload_max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    #[serde(with = "humantime_serde")]
    pub access_ttl: Duration,
    #[serde(with = "humantime_serde")]
    pub refresh_ttl: Duration,
    #[serde(with = "humantime_serde")]
    pub magic_code_ttl: Duration,
    /// Its keys sit in `[auth]` itself, where 0.1.0's `lobby.toml` has the first three.
    #[serde(flatten)]
    pub magic_code_sends: MagicCodeSends,
    /// The only email that may create the first User. Empty = nobody can bootstrap.
    pub bootstrap_email: String,
    pub join_requests_enabled: bool,
}

/// The caps on Magic Codes mailed, for `limits::CodeSends`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MagicCodeSends {
    /// Codes mailed per email and client (IPv4 address or IPv6 /48) in 10 minutes and in 24 hours.
    #[serde(rename = "magic_code_sends_per_10m")]
    pub per_email_per_10m: u32,
    #[serde(rename = "magic_code_sends_per_24h")]
    pub per_email_per_24h: u32,
    /// Codes mailed per client in 24 hours, to any emails: without it one client could mail
    /// thousands of made-up addresses a day from the Lobby's SMTP account.
    #[serde(rename = "magic_code_sends_per_client_per_24h")]
    pub per_client_per_24h: u32,
    /// Codes mailed per mailbox in 24 hours, from all clients together. `+tag` aliases count as
    /// one mailbox, so many clients cannot bomb one inbox. A Member's own codes still go out.
    #[serde(rename = "magic_code_sends_per_mailbox_per_24h")]
    pub per_mailbox_per_24h: u32,
    /// Codes mailed in 24 hours in all, so many clients together cannot use up the mail
    /// provider's quota. A Member's own codes still go out, so keep it below that quota.
    #[serde(rename = "magic_code_sends_per_lobby_per_24h")]
    pub per_lobby_per_24h: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Prefer `LOBBY_SMTP__PASSWORD` or `password_file` over the config file.
    pub password: SmtpPassword,
    /// A file holding the password (a compose secret, `LOBBY_SMTP__PASSWORD_FILE`), read once at
    /// load with trailing line breaks trimmed. Setting both it and `password` is invalid.
    pub password_file: OptionalPath,
    pub from: String,
    /// Log each Magic Code instead of mailing it, for developing without a mail provider. Allowed
    /// only in a debug build, for a server that only this machine can reach: see
    /// [`Config::validate`].
    pub log_codes: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SfuConfig {
    pub udp_bind: SocketAddr,
    /// Host candidate addresses: the public IP, plus the LAN IP for a home server.
    pub advertise_ips: Vec<IpAddr>,
    /// The port clients send media to, when a port mapping makes it differ from `udp_bind`'s.
    pub advertise_port: Option<u16>,
    /// Media over TCP, for clients whose network blocks UDP. Clients use it only when UDP fails.
    pub tcp_bind: SocketAddr,
    /// As `advertise_port`, for `tcp_bind`.
    pub tcp_advertise_port: Option<u16>,
    /// A Member whose client has sent no datagram for this long leaves Voice: its media path is
    /// gone even if its WebSocket is not.
    #[serde(with = "humantime_serde")]
    pub dead_peer_after: Duration,
    /// `dead_peer_after` while an ICE restart is under way. Never shorter than it.
    #[serde(with = "humantime_serde")]
    pub dead_peer_after_restart: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceConfig {
    pub max_participants: u32,
    pub max_sharers: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobsConfig {
    #[serde(with = "humantime_serde")]
    pub cleanup_interval: Duration,
}

const MIB: u64 = 1024 * 1024;
/// A live client sends a STUN consent check at least every 3 s, besides RTP and RTCP.
pub const DEAD_PEER_AFTER: Duration = Duration::from_secs(15);
/// Every restart a client tries, plus room for its last one to be heard.
pub const DEAD_PEER_AFTER_RESTART: Duration = RestartPolicy::DEFAULT
    .window()
    .saturating_add(Duration::from_secs(5));

impl Default for Config {
    fn default() -> Self {
        Self {
            shutdown_timeout: Duration::from_secs(10),
            lobby: LobbyConfig {
                name: "Lobby".into(),
            },
            http: HttpConfig {
                bind: SocketAddr::from(([0, 0, 0, 0], 8080)),
                trusted_proxy_cidrs: Vec::new(),
                rate_limits: true,
            },
            ws: WsConfig {
                identify_timeout: Duration::from_secs(10),
                ping_interval: Duration::from_secs(10),
                pong_timeout: Duration::from_secs(15),
                write_timeout: Duration::from_secs(10),
                outbound_queue: 256,
                max_frame_bytes: 65_536,
                max_unidentified_per_ip: 20,
                max_unidentified_global: 200,
                send_every: Duration::from_millis(200),
            },
            database: DatabaseConfig {
                path: "./data/lobby.db".into(),
            },
            storage: StorageConfig {
                blob_root: "./data/blobs".into(),
                upload_max_bytes: 10 * MIB,
            },
            auth: AuthConfig {
                access_ttl: Duration::from_secs(15 * 60),
                refresh_ttl: Duration::from_secs(30 * 24 * 3600),
                magic_code_ttl: Duration::from_secs(10 * 60),
                magic_code_sends: MagicCodeSends {
                    per_email_per_10m: 3,
                    per_email_per_24h: 10,
                    // A friend group behind one NAT signing in on a fresh server: a dozen friends
                    // on three devices need 36 codes, with room for a few resends.
                    per_client_per_24h: 50,
                    per_mailbox_per_24h: 20,
                    // The first day of a dozen friends on three devices is 36 codes. Keep this
                    // below the mail provider's daily quota (a free plan may allow 100).
                    per_lobby_per_24h: 80,
                },
                bootstrap_email: String::new(),
                join_requests_enabled: true,
            },
            smtp: SmtpConfig {
                host: String::new(),
                port: 587,
                username: String::new(),
                password: SmtpPassword::default(),
                password_file: OptionalPath(None),
                from: String::new(),
                log_codes: false,
            },
            sfu: SfuConfig {
                udp_bind: SocketAddr::from(([0, 0, 0, 0], 50000)),
                advertise_ips: Vec::new(),
                advertise_port: None,
                tcp_bind: SocketAddr::from(([0, 0, 0, 0], 50000)),
                tcp_advertise_port: None,
                dead_peer_after: DEAD_PEER_AFTER,
                dead_peer_after_restart: DEAD_PEER_AFTER_RESTART,
            },
            voice: VoiceConfig {
                max_participants: 25,
                max_sharers: 4,
            },
            jobs: JobsConfig {
                cleanup_interval: Duration::from_secs(3600),
            },
        }
    }
}

#[derive(Debug, Clone)]
pub enum ConfigSource {
    Default,
    File(PathBuf),
    None,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file {} does not exist", .0.display())]
    MissingFile(PathBuf),
    #[error("config could not be read: {0}")]
    Parse(Box<figment::Error>),
    #[error("invalid config:\n  - {}", .0.join("\n  - "))]
    Invalid(Vec<String>),
    #[error("cannot read {key} {}: {source}", .path.display())]
    SecretFile {
        key: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Config {
    pub const DEFAULT_FILE: &'static str = "lobby.toml";

    pub fn figment(source: &ConfigSource) -> Figment {
        let mut figment = Figment::from(Serialized::defaults(Config::default()));
        // `file_exact` because `Toml::file` would also search parent directories; it reports a
        // missing file as an error, so the optional default file is checked first.
        match source {
            ConfigSource::Default if Path::new(Self::DEFAULT_FILE).is_file() => {
                figment = figment.merge(Toml::file_exact(Self::DEFAULT_FILE));
            }
            ConfigSource::File(path) => figment = figment.merge(Toml::file_exact(path)),
            ConfigSource::Default | ConfigSource::None => {}
        }
        figment.merge(Env::prefixed("LOBBY_").ignore(NON_CONFIG_ENV).split("__"))
    }

    pub fn load(source: &ConfigSource) -> Result<Config, ConfigError> {
        if let ConfigSource::File(path) = source
            && !path.is_file()
        {
            return Err(ConfigError::MissingFile(path.clone()));
        }
        let mut cfg: Config = Self::figment(source)
            .extract()
            .map_err(|e| ConfigError::Parse(Box::new(e)))?;
        cfg.resolve_secret_files()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reads `smtp.password_file` into `smtp.password`, so the secret never has to be in the
    /// environment or the config file.
    fn resolve_secret_files(&mut self) -> Result<(), ConfigError> {
        let Some(path) = self.smtp.password_file.0.clone() else {
            return Ok(());
        };
        if !self.smtp.password.expose().is_empty() {
            return Err(ConfigError::Invalid(vec![
                "smtp.password and smtp.password_file are both set: keep one".to_owned(),
            ]));
        }
        let secret = std::fs::read_to_string(&path).map_err(|source| ConfigError::SecretFile {
            key: "smtp.password_file",
            path,
            source,
        })?;
        self.smtp.password = SmtpPassword::new(secret.trim_end_matches(['\r', '\n']));
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errs = Vec::new();
        let mut check = |ok: bool, msg: &str| {
            if !ok {
                errs.push(msg.to_owned());
            }
        };
        check(!self.lobby.name.trim().is_empty(), "lobby.name is required");
        if self.smtp.log_codes {
            check(
                cfg!(debug_assertions),
                "smtp.log_codes works only in a debug build: a deployed Lobby needs a mail \
                 provider",
            );
            // A deployed server sits behind a proxy, even when it binds 127.0.0.1 for it.
            check(
                self.http.bind.ip().is_loopback() && self.http.trusted_proxy_cidrs.is_empty(),
                "smtp.log_codes writes Magic Codes to the log: it needs http.bind on this machine \
                 and no http.trusted_proxy_cidrs. A deployed Lobby needs a mail provider",
            );
        } else {
            check(!self.smtp.host.trim().is_empty(), "smtp.host is required");
            check(self.smtp.port != 0, "smtp.port is required");
            check(!self.smtp.from.trim().is_empty(), "smtp.from is required");
            if !self.smtp.from.trim().is_empty() {
                check(
                    self.smtp.from.parse::<lettre::message::Mailbox>().is_ok(),
                    "smtp.from is not a valid mailbox (\"Lobby <noreply@example.com>\")",
                );
            }
            check(
                self.smtp.username.is_empty()
                    || is_this_machine(&self.smtp.host)
                    || tls_policy(&self.smtp.host, self.smtp.port) == TlsPolicy::Required,
                "smtp.username is set but smtp.host is on port 1025 off this machine, where the \
                 password could cross the network unencrypted: use a STARTTLS port such as 587",
            );
        }
        check(
            !self.sfu.advertise_ips.is_empty(),
            "sfu.advertise_ips must list at least one address",
        );
        check(
            self.sfu.advertise_port != Some(0),
            "sfu.advertise_port must not be 0",
        );
        check(
            self.sfu.tcp_advertise_port != Some(0),
            "sfu.tcp_advertise_port must not be 0",
        );
        let sends = self.auth.magic_code_sends;
        for (name, cap) in [
            ("auth.magic_code_sends_per_10m", sends.per_email_per_10m),
            ("auth.magic_code_sends_per_24h", sends.per_email_per_24h),
            (
                "auth.magic_code_sends_per_client_per_24h",
                sends.per_client_per_24h,
            ),
            (
                "auth.magic_code_sends_per_mailbox_per_24h",
                sends.per_mailbox_per_24h,
            ),
            (
                "auth.magic_code_sends_per_lobby_per_24h",
                sends.per_lobby_per_24h,
            ),
        ] {
            check(cap > 0, &format!("{name} must be greater than 0"));
        }
        for (name, ttl) in [
            ("auth.access_ttl", self.auth.access_ttl),
            ("auth.refresh_ttl", self.auth.refresh_ttl),
            ("auth.magic_code_ttl", self.auth.magic_code_ttl),
            ("shutdown_timeout", self.shutdown_timeout),
            ("ws.identify_timeout", self.ws.identify_timeout),
            ("ws.ping_interval", self.ws.ping_interval),
            ("ws.pong_timeout", self.ws.pong_timeout),
            ("ws.write_timeout", self.ws.write_timeout),
            ("jobs.cleanup_interval", self.jobs.cleanup_interval),
            ("sfu.dead_peer_after", self.sfu.dead_peer_after),
            (
                "sfu.dead_peer_after_restart",
                self.sfu.dead_peer_after_restart,
            ),
        ] {
            check(!ttl.is_zero(), &format!("{name} must be greater than 0"));
        }
        check(
            self.sfu.dead_peer_after_restart >= self.sfu.dead_peer_after,
            "sfu.dead_peer_after_restart must not be shorter than sfu.dead_peer_after",
        );
        check(
            self.ws.outbound_queue > 0,
            "ws.outbound_queue must be greater than 0",
        );
        check(
            self.ws.max_frame_bytes > 0,
            "ws.max_frame_bytes must be greater than 0",
        );
        check(
            (MIB..=1024 * MIB).contains(&self.storage.upload_max_bytes),
            "storage.upload_max_bytes must be between 1 MiB and 1 GiB",
        );
        check(
            !self.database.path.as_os_str().is_empty(),
            "database.path is required",
        );
        if !self.auth.bootstrap_email.trim().is_empty() {
            check(
                Email::parse(&self.auth.bootstrap_email).is_ok(),
                "auth.bootstrap_email is not a valid email",
            );
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(errs))
        }
    }

    /// Valid but risky settings, logged at `warn` on startup.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !self.http.rate_limits {
            warnings.push(
                "http.rate_limits is off: Sign In, Magic Code and join requests are not limited per IP, nor Message sends per Member"
                    .to_owned(),
            );
        }
        if self.smtp.log_codes {
            warnings.push(
                "smtp.log_codes is on: Magic Codes are written to this log and nothing is mailed"
                    .to_owned(),
            );
        }
        if self.shutdown_timeout >= Duration::from_secs(10) {
            warnings.push(format!(
                "shutdown_timeout is {}: the container stop grace period must exceed it \
                 (compose ships 20s; Docker's default is 10s)",
                humantime_serde::re::humantime::format_duration(self.shutdown_timeout)
            ));
        }
        let only_local_proxy = !self.http.trusted_proxy_cidrs.is_empty()
            && self
                .http
                .trusted_proxy_cidrs
                .iter()
                .all(|c| c.addr().is_loopback());
        if only_local_proxy && !self.http.bind.ip().is_loopback() {
            warnings.push(
                "http.bind is public but the only trusted proxy is local: bind 127.0.0.1:8080 \
                 behind Caddy"
                    .to_owned(),
            );
        }
        warnings
    }
}

/// A path that may be left unset with `""`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct OptionalPath(pub Option<PathBuf>);

impl From<String> for OptionalPath {
    fn from(s: String) -> Self {
        let s = s.trim();
        Self((!s.is_empty()).then(|| PathBuf::from(s)))
    }
}

impl From<OptionalPath> for String {
    fn from(p: OptionalPath) -> String {
        p.0.map(|p| p.display().to_string()).unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // figment::Jail's closure signature
mod tests {
    use figment::Jail;

    use super::*;

    const VALID: &str = r#"
        [smtp]
        host = "mail.example.com"
        from = "Lobby <noreply@example.com>"
        [sfu]
        advertise_ips = ["203.0.113.7"]
    "#;

    fn load_file(jail: &mut Jail, toml: &str) -> Result<Config, ConfigError> {
        jail.create_file("lobby.toml", toml).unwrap();
        Config::load(&ConfigSource::File("lobby.toml".into()))
    }

    fn invalid(result: Result<Config, ConfigError>) -> Vec<String> {
        match result {
            Err(ConfigError::Invalid(errs)) => errs,
            other => panic!("expected validation errors, got {other:?}"),
        }
    }

    #[test]
    fn unknown_keys_are_rejected_in_the_file_and_the_env() {
        Jail::expect_with(|jail| {
            let err = load_file(jail, &format!("{VALID}\n[http]\nbnid = \"x\"\n")).unwrap_err();
            assert!(matches!(err, ConfigError::Parse(_)), "{err}");
            assert!(err.to_string().contains("bnid"), "{err}");
            Ok(())
        });
        Jail::expect_with(|jail| {
            jail.set_env("LOBBY_SMTP__HOTS", "typo.example.com");
            let err = load_file(jail, VALID).unwrap_err();
            assert!(err.to_string().contains("hots"), "{err}");
            Ok(())
        });
        Jail::expect_with(|jail| {
            // The Magic Code caps are flattened into `[auth]`, which still rejects a typo.
            jail.set_env("LOBBY_AUTH__MAGIC_CODE_SENDS_PER_10M", "5");
            let cfg = load_file(jail, VALID).unwrap();
            assert_eq!(cfg.auth.magic_code_sends.per_email_per_10m, 5);
            jail.set_env("LOBBY_AUTH__MAGIC_CODE_SENDS_PER_1OM", "5");
            let err = load_file(jail, VALID).unwrap_err();
            assert!(
                err.to_string().contains("magic_code_sends_per_1om"),
                "{err}"
            );
            Ok(())
        });
        Jail::expect_with(|jail| {
            jail.create_file("lobby.toml", VALID)?;
            let err = Config::load(&ConfigSource::File("lobby.toml".into()));
            assert!(err.is_ok(), "{err:?}");
            jail.set_env("LOBBY_TOP_LEVEL_TYPO", "1");
            let err = Config::load(&ConfigSource::File("lobby.toml".into())).unwrap_err();
            assert!(err.to_string().contains("top_level_typo"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn every_validation_rule_is_reported_at_once() {
        Jail::expect_with(|jail| {
            let errs = invalid(load_file(
                jail,
                r#"
                [lobby]
                name = "  "
                [ws]
                outbound_queue = 0
                max_frame_bytes = 0
                identify_timeout = "0s"
                [storage]
                upload_max_bytes = 1048575
                [auth]
                access_ttl = "0s"
                refresh_ttl = "0s"
                magic_code_ttl = "0s"
                magic_code_sends_per_10m = 0
                magic_code_sends_per_24h = 0
                magic_code_sends_per_client_per_24h = 0
                magic_code_sends_per_mailbox_per_24h = 0
                magic_code_sends_per_lobby_per_24h = 0
                bootstrap_email = "not-an-email"
                [sfu]
                advertise_port = 0
                tcp_advertise_port = 0
                dead_peer_after = "0s"
                "#,
            ));
            let expected = [
                "lobby.name is required",
                "smtp.host is required",
                "smtp.from is required",
                "sfu.advertise_ips must list at least one address",
                "sfu.advertise_port must not be 0",
                "sfu.tcp_advertise_port must not be 0",
                "auth.magic_code_sends_per_10m must be greater than 0",
                "auth.magic_code_sends_per_24h must be greater than 0",
                "auth.magic_code_sends_per_client_per_24h must be greater than 0",
                "auth.magic_code_sends_per_mailbox_per_24h must be greater than 0",
                "auth.magic_code_sends_per_lobby_per_24h must be greater than 0",
                "auth.access_ttl must be greater than 0",
                "auth.refresh_ttl must be greater than 0",
                "auth.magic_code_ttl must be greater than 0",
                "ws.identify_timeout must be greater than 0",
                "sfu.dead_peer_after must be greater than 0",
                "ws.outbound_queue must be greater than 0",
                "ws.max_frame_bytes must be greater than 0",
                "storage.upload_max_bytes must be between 1 MiB and 1 GiB",
                "auth.bootstrap_email is not a valid email",
            ];
            assert_eq!(errs, expected);
            Ok(())
        });
    }

    #[test]
    fn credentials_are_refused_for_plain_smtp_off_this_machine() {
        let smtp = |host: &str, port: u16| {
            format!(
                r#"
                [smtp]
                host = "{host}"
                port = {port}
                username = "key"
                from = "Lobby <noreply@example.com>"
                [sfu]
                advertise_ips = ["203.0.113.7"]
                "#
            )
        };
        Jail::expect_with(|jail| {
            let errs = invalid(load_file(jail, &smtp("mailpit", 1025)));
            assert_eq!(errs.len(), 1, "{errs:?}");
            assert!(errs[0].starts_with("smtp.username is set"), "{errs:?}");
            for (host, port) in [("127.0.0.1", 1025), ("mail.example.com", 25)] {
                load_file(jail, &smtp(host, port)).unwrap();
            }
            Ok(())
        });
    }

    #[test]
    fn log_codes_replace_the_mail_settings_only_for_a_server_no_proxy_fronts() {
        let local = r#"
            [smtp]
            log_codes = true
            [http]
            bind = "127.0.0.1:8080"
            [sfu]
            advertise_ips = ["127.0.0.1"]
        "#;
        Jail::expect_with(|jail| {
            load_file(jail, local).unwrap();
            for http in [
                "bind = \"0.0.0.0:8080\"",
                "bind = \"127.0.0.1:8080\"\ntrusted_proxy_cidrs = [\"127.0.0.1/32\"]",
            ] {
                let toml = local.replace("bind = \"127.0.0.1:8080\"", http);
                let errs = invalid(load_file(jail, &toml));
                assert_eq!(errs.len(), 1, "{errs:?}");
                assert!(errs[0].starts_with("smtp.log_codes"), "{errs:?}");
            }
            Ok(())
        });
    }

    #[test]
    fn a_restart_deadline_shorter_than_the_silence_deadline_is_refused() {
        Jail::expect_with(|jail| {
            let errs = invalid(load_file(
                jail,
                r#"
                [sfu]
                advertise_ips = ["127.0.0.1"]
                dead_peer_after = "20s"
                dead_peer_after_restart = "10s"
                "#,
            ));
            assert!(
                errs.contains(
                    &"sfu.dead_peer_after_restart must not be shorter than sfu.dead_peer_after"
                        .to_owned()
                ),
                "{errs:?}"
            );
            Ok(())
        });
    }

    #[test]
    fn secrets_are_not_in_debug_output() {
        let mut c = Config::default();
        c.smtp.password = SmtpPassword::new("hunter2-hunter2");
        assert!(!format!("{c:?}").contains("hunter2"));
    }

    fn repo_file(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    }

    #[test]
    fn the_example_file_lists_exactly_the_defaults() {
        let from_file: Config =
            Figment::from(Toml::file_exact(repo_file("deploy/lobby.example.toml")))
                .extract()
                .unwrap();
        assert_eq!(from_file, Config::default());
    }
}
