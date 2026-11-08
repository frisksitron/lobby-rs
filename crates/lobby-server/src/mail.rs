//! Outgoing mail: one tracked task per Magic Code, three attempts with jittered backoff, a timeout
//! per attempt, at most four sends at once. The HTTP response never waits for it. STARTTLS is
//! required, except for a relay on this machine or on port 1025 (a dev sink such as mailpit), which
//! use STARTTLS when the server offers it and plain SMTP otherwise. Config refuses credentials for
//! a relay on port 1025 off this machine. Logs name the recipient's domain only, except with
//! `smtp.log_codes`, which logs the code itself in place of mailing it.

use std::{sync::Arc, time::Duration};

use lettre::{
    AsyncSmtpTransport, AsyncTransport as _, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
    transport::smtp::{
        authentication::Credentials,
        client::{Certificate, Tls, TlsParameters},
    },
};
use lobby_protocol::{Email, MagicCode, RequestLabel};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::{Instrument as _, info, info_span, warn};

use crate::config::SmtpConfig;

pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);
pub const BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];
pub const ATTEMPTS: usize = 3;
pub const CONCURRENT: usize = 4;
/// The port of a dev sink such as mailpit, which may be in another container: STARTTLS only if
/// offered.
pub const DEV_SINK_PORT: u16 = 1025;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsPolicy {
    /// STARTTLS or nothing: credentials and codes never cross a network in the clear.
    Required,
    /// STARTTLS when the relay advertises it, else plain SMTP: a relay on this machine or a dev
    /// sink on [`DEV_SINK_PORT`].
    Opportunistic,
}

pub fn is_this_machine(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Port 25 of another host is `Required` like any other: a network attacker could otherwise strip
/// the STARTTLS offer and read the credentials and the codes.
pub fn tls_policy(host: &str, port: u16) -> TlsPolicy {
    if is_this_machine(host) || port == DEV_SINK_PORT {
        TlsPolicy::Opportunistic
    } else {
        TlsPolicy::Required
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("smtp.from is not a mailbox: {0}")]
    From(String),
    #[error("smtp transport: {0}")]
    Transport(String),
}

#[derive(Clone)]
pub struct Mailer {
    /// `None` with `smtp.log_codes`: each code goes to the log and nothing is mailed.
    relay: Option<Relay>,
    tracker: TaskTracker,
    stop: CancellationToken,
}

#[derive(Clone)]
struct Relay {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    lobby_name: String,
    ttl_minutes: u64,
    permits: Arc<Semaphore>,
}

impl Mailer {
    /// `tracker` and `stop` are the services tier's: shutdown waits for sends in flight and
    /// cancels their retries.
    pub fn new(
        cfg: &SmtpConfig,
        lobby_name: &str,
        code_ttl: Duration,
        tracker: TaskTracker,
        stop: CancellationToken,
    ) -> Result<Mailer, MailError> {
        Self::build(cfg, lobby_name, code_ttl, tracker, stop, None)
    }

    /// [`Mailer::new`] trusting one more root certificate (a test's STARTTLS sink).
    #[cfg(feature = "test-support")]
    pub fn with_root_certificate(
        cfg: &SmtpConfig,
        lobby_name: &str,
        code_ttl: Duration,
        tracker: TaskTracker,
        stop: CancellationToken,
        pem: &[u8],
    ) -> Result<Mailer, MailError> {
        Self::build(cfg, lobby_name, code_ttl, tracker, stop, Some(pem))
    }

    fn build(
        cfg: &SmtpConfig,
        lobby_name: &str,
        code_ttl: Duration,
        tracker: TaskTracker,
        stop: CancellationToken,
        extra_root: Option<&[u8]>,
    ) -> Result<Mailer, MailError> {
        if cfg.log_codes {
            return Ok(Mailer {
                relay: None,
                tracker,
                stop,
            });
        }
        let from: Mailbox = cfg
            .from
            .parse()
            .map_err(|e: lettre::address::AddressError| MailError::From(e.to_string()))?;
        let transport_err = |e: lettre::transport::smtp::Error| MailError::Transport(e.to_string());
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(cfg.host.trim())
            .port(cfg.port)
            .timeout(Some(ATTEMPT_TIMEOUT));
        let mut tls = TlsParameters::builder(cfg.host.trim().to_owned());
        if let Some(pem) = extra_root {
            tls = tls.add_root_certificate(Certificate::from_pem(pem).map_err(transport_err)?);
        }
        let tls = tls.build_rustls().map_err(transport_err)?;
        builder = builder.tls(match tls_policy(&cfg.host, cfg.port) {
            TlsPolicy::Required => Tls::Required(tls),
            TlsPolicy::Opportunistic => Tls::Opportunistic(tls),
        });
        if !cfg.username.is_empty() {
            builder = builder.credentials(Credentials::new(
                cfg.username.clone(),
                cfg.password.expose().to_owned(),
            ));
        }
        Ok(Mailer {
            relay: Some(Relay {
                transport: builder.build(),
                from,
                lobby_name: lobby_name.to_owned(),
                ttl_minutes: code_ttl.as_secs().div_ceil(60).max(1),
                permits: Arc::new(Semaphore::new(CONCURRENT)),
            }),
            tracker,
            stop,
        })
    }

    /// Connects and signs in once in the background, and logs whether it worked, so a wrong host,
    /// port or key shows at start and not as a friend's code that never arrives. No retries: a
    /// relay that is down now can be up when a code is sent.
    pub fn check_reachable(&self, cfg: &SmtpConfig) {
        let Some(relay) = &self.relay else {
            return;
        };
        let (transport, stop) = (relay.transport.clone(), self.stop.clone());
        let (host, port) = (cfg.host.trim().to_owned(), cfg.port);
        self.tracker.spawn(async move {
            const NOT_ARRIVING: &str = "Magic Code mail will not arrive";
            let check = tokio::time::timeout(ATTEMPT_TIMEOUT, transport.test_connection());
            tokio::select! {
                () = stop.cancelled() => {}
                done = check => match done {
                    Ok(Ok(true)) => info!(host, port, "smtp relay reachable"),
                    Ok(Ok(false)) => warn!(host, port, "smtp relay did not answer: {NOT_ARRIVING}"),
                    Ok(Err(e)) => warn!(host, port, error = %e, "smtp relay not usable: {NOT_ARRIVING}"),
                    Err(_) => warn!(host, port, "smtp relay timed out: {NOT_ARRIVING}"),
                }
            }
        });
    }

    pub fn send_code(&self, to: &Email, code: &MagicCode, label: &RequestLabel) {
        let Some(relay) = &self.relay else {
            info!(
                domain = to.domain(),
                code = code.expose(),
                %label,
                "magic code, not mailed"
            );
            return;
        };
        let message = match relay.code_message(to, code, label) {
            Ok(m) => m,
            Err(e) => {
                warn!(domain = to.domain(), error = %e, "magic code mail not built");
                return;
            }
        };
        let (transport, permits, stop) = (
            relay.transport.clone(),
            relay.permits.clone(),
            self.stop.clone(),
        );
        let span = info_span!("mail", domain = to.domain());
        self.tracker.spawn(
            async move {
                tokio::select! {
                    () = stop.cancelled() => warn!("magic code mail abandoned: shutting down"),
                    () = deliver(&transport, &permits, &message) => {}
                }
            }
            .instrument(span),
        );
    }
}

impl Relay {
    fn code_message(
        &self,
        to: &Email,
        code: &MagicCode,
        label: &RequestLabel,
    ) -> Result<Message, lettre::error::Error> {
        let to: Mailbox = to
            .as_str()
            .parse()
            .map_err(|_| lettre::error::Error::MissingTo)?;
        Message::builder()
            .from(self.from.clone())
            .to(to)
            // The code leads, so a notification preview shows it.
            .subject(subject(code, &self.lobby_name, label))
            .header(ContentType::TEXT_PLAIN)
            .body(format!(
                "Your code for {} is {}.\n\
                 It is for request {label} and expires in {} minutes.\n\n\
                 If you didn't ask for it, you can ignore this email.",
                self.lobby_name,
                code.expose(),
                self.ttl_minutes
            ))
    }
}

/// "8042 1579 is your code for Friends (request KQV)": two groups of four, as the app's code
/// cells show it, and the request the app names beside them.
fn subject(code: &MagicCode, lobby: &str, label: &RequestLabel) -> String {
    let digits = code.expose();
    let (a, b) = digits.split_at(digits.len() / 2);
    format!("{a} {b} is your code for {lobby} (request {label})")
}

async fn deliver(
    transport: &AsyncSmtpTransport<Tokio1Executor>,
    permits: &Semaphore,
    message: &Message,
) {
    let Ok(_permit) = permits.acquire().await else {
        return;
    };
    for attempt in 1..=ATTEMPTS {
        match tokio::time::timeout(ATTEMPT_TIMEOUT, transport.send(message.clone())).await {
            Ok(Ok(_)) => {
                info!(attempt, "magic code mail sent");
                return;
            }
            Ok(Err(e)) if attempt < ATTEMPTS => warn!(attempt, error = %e, "mail send failed"),
            Err(_) if attempt < ATTEMPTS => warn!(attempt, "mail send timed out"),
            Ok(Err(e)) => warn!(attempt, error = %e, "magic code mail not delivered"),
            Err(_) => warn!(attempt, "magic code mail not delivered: timed out"),
        }
        if let Some(base) = BACKOFF.get(attempt - 1) {
            tokio::time::sleep(jittered(*base)).await;
        }
    }
}

fn jittered(base: Duration) -> Duration {
    base.mul_f64(rand::random_range(0.8..=1.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starttls_is_required_except_on_this_machine_and_the_dev_sink_port() {
        assert_eq!(tls_policy("smtp.example.com", 587), TlsPolicy::Required);
        assert_eq!(tls_policy("192.168.1.10", 587), TlsPolicy::Required);
        for (host, port) in [
            ("127.0.0.1", 587),
            ("localhost", 2525),
            ("[::1]", 587),
            ("mailpit", 1025),
        ] {
            assert_eq!(
                tls_policy(host, port),
                TlsPolicy::Opportunistic,
                "{host}:{port}"
            );
        }
        assert_eq!(tls_policy("relay.example.com", 25), TlsPolicy::Required);
    }
}
