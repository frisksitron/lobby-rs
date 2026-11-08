//! The Magic Code mail over STARTTLS: lettre upgrades a local relay that offers STARTTLS, verifies
//! its certificate, and delivers over TLS; with a certificate it cannot verify, nothing is
//! delivered. The relay's certificate (`tests/integration/fixtures/smtp-tls`) is signed by a test
//! CA valid until 2126.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use lobby_protocol::{Email, MagicCode, RequestLabel, secret::SmtpPassword};
use lobby_server::{config::SmtpConfig, mail::Mailer};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader},
    net::TcpListener,
};
use tokio_rustls::{TlsAcceptor, rustls};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const CA: &[u8] = include_bytes!("fixtures/smtp-tls/ca.pem");
const CERT: &[u8] = include_bytes!("fixtures/smtp-tls/cert.pem");
const KEY: &[u8] = include_bytes!("fixtures/smtp-tls/key.pem");

#[derive(Debug, Default)]
struct Seen {
    starttls: bool,
    over_tls: Vec<String>,
    plain: Vec<String>,
}

fn acceptor() -> TlsAcceptor {
    let certs = CertificateDer::pem_slice_iter(CERT)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(KEY).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

/// One SMTP session until QUIT or EOF, each message handed to `got` as it arrives (a pooled
/// client keeps the session open); `None` when the client asked for STARTTLS (the caller
/// upgrades and runs the rest of the session over TLS).
async fn session<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    offer_tls: bool,
    greet: bool,
    got: impl Fn(String),
) -> std::io::Result<Option<()>> {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    if greet {
        write.write_all(b"220 tls-sink ESMTP\r\n").await?;
    }
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line).await? == 0 {
            return Ok(Some(()));
        }
        let verb = line.trim_end().to_ascii_uppercase();
        if verb.starts_with("EHLO") {
            let ehlo: &[u8] = if offer_tls {
                b"250-tls-sink\r\n250 STARTTLS\r\n"
            } else {
                b"250 tls-sink\r\n"
            };
            write.write_all(ehlo).await?;
        } else if verb == "STARTTLS" && offer_tls {
            write.write_all(b"220 Go ahead\r\n").await?;
            return Ok(None);
        } else if verb == "DATA" {
            write.write_all(b"354 go\r\n").await?;
            let mut data = String::new();
            loop {
                line.clear();
                if lines.read_line(&mut line).await? == 0 {
                    return Ok(Some(()));
                }
                if line.trim_end() == "." {
                    break;
                }
                data.push_str(&line);
            }
            got(data);
            write.write_all(b"250 queued\r\n").await?;
        } else if verb == "QUIT" {
            write.write_all(b"221 bye\r\n").await?;
            return Ok(Some(()));
        } else {
            write.write_all(b"250 OK\r\n").await?;
        }
    }
}

async fn tls_sink() -> (u16, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = seen.clone();
    let acceptor = acceptor();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let (record, acceptor) = (record.clone(), acceptor.clone());
            tokio::spawn(async move {
                let plain = |m| record.lock().unwrap().plain.push(m);
                if let Ok(None) = session(&mut tcp, true, true, plain).await {
                    record.lock().unwrap().starttls = true;
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let over_tls = |m| record.lock().unwrap().over_tls.push(m);
                    let _ = session(&mut tls, false, false, over_tls).await;
                }
            });
        }
    });
    (port, seen)
}

fn smtp(port: u16) -> SmtpConfig {
    SmtpConfig {
        host: "localhost".into(),
        port,
        username: String::new(),
        password: SmtpPassword::default(),
        password_file: Default::default(),
        from: "Lobby <noreply@lobby.test>".into(),
        log_codes: false,
    }
}

fn label() -> RequestLabel {
    RequestLabel::try_from("KQV".to_owned()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_code_goes_out_over_starttls_with_a_verified_certificate() {
    let (port, seen) = tls_sink().await;
    let tracker = TaskTracker::new();
    let mailer = Mailer::with_root_certificate(
        &smtp(port),
        "Friends",
        Duration::from_secs(600),
        tracker.clone(),
        CancellationToken::new(),
        CA,
    )
    .unwrap();
    let to = Email::parse("carol@example.com").unwrap();
    mailer.send_code(&to, &MagicCode::new("04821379").unwrap(), &label());
    tracker.close();
    tokio::time::timeout(Duration::from_secs(20), tracker.wait())
        .await
        .expect("the send finished");
    let seen = seen.lock().unwrap();
    assert!(seen.starttls, "{seen:?}");
    assert!(seen.plain.is_empty(), "nothing in the clear: {seen:?}");
    assert_eq!(seen.over_tls.len(), 1, "{seen:?}");
    let mail = &seen.over_tls[0];
    assert!(
        mail.contains("Subject: 0482 1379 is your code for Friends (request KQV)"),
        "{mail}"
    );
    assert!(
        mail.contains(
            "Your code for Friends is 04821379.\r\n\
             It is for request KQV and expires in 10 minutes.\r\n\r\n\
             If you didn't ask for it"
        ),
        "{mail}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_certificate_it_cannot_verify_gets_no_code() {
    let (port, seen) = tls_sink().await;
    // The public roots only: the test CA is unknown.
    let mailer = Mailer::new(
        &smtp(port),
        "Friends",
        Duration::from_secs(600),
        TaskTracker::new(),
        CancellationToken::new(),
    )
    .unwrap();
    mailer.send_code(
        &Email::parse("carol@example.com").unwrap(),
        &MagicCode::new("04821379").unwrap(),
        &label(),
    );
    for _ in 0..100 {
        if seen.lock().unwrap().starttls {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Long enough for a handshake that would succeed to deliver.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let seen = seen.lock().unwrap();
    assert!(seen.starttls, "the upgrade was attempted: {seen:?}");
    assert!(
        seen.over_tls.is_empty() && seen.plain.is_empty(),
        "{seen:?}"
    );
}
