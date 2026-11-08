//! In-process test server. It adds no routes; Members are written straight into the database before
//! the server starts ([`seed_members`]) or created through the public endpoints. Every test server
//! mails into its own [`SmtpSink`], so a test reads Magic Codes the way a person reads their inbox.
//! [`MediaRelay`] stands between clients' media (UDP and TCP) and the SFU and breaks or narrows
//! that path on demand.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::SocketAddr,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use lobby_protocol::{
    Attachment, Email, MagicCode, SignedIn, UnixMs, UserId, Username, VerifyOutcome,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Notify, mpsc},
};
use tokio_util::task::AbortOnDropHandle;

pub use crate::domain::FRONT_DOOR_WRITES;
use crate::{Config, RunningServer, ShutdownReport, Sockets, db::users::Profile, start};

const MAIL_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn test_config(data_dir: &Path) -> Config {
    let mut cfg = Config::default();
    cfg.lobby.name = "Test Lobby".into();
    cfg.http.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    cfg.database.path = data_dir.join("lobby.db");
    cfg.storage.blob_root = data_dir.join("blobs");
    cfg.smtp.host = "127.0.0.1".into();
    cfg.smtp.port = 1025;
    cfg.smtp.from = "Lobby <noreply@lobby.test>".into();
    cfg.sfu.udp_bind = SocketAddr::from(([127, 0, 0, 1], 0));
    cfg.sfu.tcp_bind = SocketAddr::from(([127, 0, 0, 1], 0));
    cfg.sfu.advertise_ips = vec![[127, 0, 0, 1].into()];
    cfg.shutdown_timeout = Duration::from_secs(5);
    // Tests sign many Members in from one address and one inbox; the limits have their own tests.
    cfg.http.rate_limits = false;
    cfg.auth.magic_code_sends_per_10m = 1000;
    cfg.auth.magic_code_sends_per_24h = 1000;
    cfg.auth.magic_code_sends_per_client_per_24h = 1000;
    cfg
}

const TEST_LOG_FILTER: &str = "warn";

/// The server's warnings and errors land in the output of a failing test, which nextest prints.
/// `RUST_LOG` widens it. Installed once per process; later calls find it taken and do nothing.
fn install_test_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(TEST_LOG_FILTER));
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(filter)
        .try_init();
}

pub struct TestServer {
    pub server: RunningServer,
    pub addr: SocketAddr,
    pub config: Config,
    pub mail: SmtpSink,
    dir: TempDir,
}

impl TestServer {
    pub async fn start() -> TestServer {
        Self::start_with(|_| {}).await
    }

    /// `tweak` edits the [`test_config`] before validation. Listeners bind port 0, so tests never
    /// race for a port.
    pub async fn start_with(tweak: impl FnOnce(&mut Config)) -> TestServer {
        Self::start_with_members(&[], tweak).await
    }

    pub async fn start_with_members(names: &[&str], tweak: impl FnOnce(&mut Config)) -> TestServer {
        Self::start_with_history(names, None, tweak).await
    }

    /// As [`Self::start_with_members`], plus `history = (author, count)` Messages by that Member
    /// already stored (oldest first, three minutes apart, the newest three minutes ago), the way a
    /// Lobby looks after weeks of use.
    pub async fn start_with_history(
        names: &[&str],
        history: Option<(&str, usize)>,
        tweak: impl FnOnce(&mut Config),
    ) -> TestServer {
        let dir = tempfile::tempdir().expect("tempdir");
        let mail = SmtpSink::start().await;
        let mut config = test_config(dir.path());
        config.smtp.port = mail.port();
        tweak(&mut config);
        config.validate().expect("test config is valid");
        let profiles = seed_members(&config, names).await;
        if let Some((author, count)) = history {
            let author = profiles
                .iter()
                .find(|p| p.username.as_str() == author)
                .expect("the history's author is one of the Members");
            seed_history(&config, &author.id, count).await;
        }
        Self::launch(config, dir, mail).await
    }

    async fn launch(config: Config, dir: TempDir, mail: SmtpSink) -> TestServer {
        install_test_logging();
        let tcp = TcpListener::bind(config.http.bind)
            .await
            .expect("bind http");
        let udp = crate::bind_udp(config.sfu.udp_bind).expect("bind udp");
        let sfu_tcp = crate::bind_tcp(config.sfu.tcp_bind).expect("bind sfu tcp");
        let server = start(config.clone(), Sockets { tcp, udp, sfu_tcp })
            .await
            .expect("server starts");
        let addr = server.local_addr();
        TestServer {
            server,
            addr,
            config,
            mail,
            dir,
        }
    }

    /// Ctrl+C and start again on the same port and database, as a restart would.
    pub async fn restart(self) -> (ShutdownReport, TestServer) {
        let TestServer {
            server,
            addr,
            mut config,
            mail,
            dir,
        } = self;
        let report = server.shutdown().await;
        config.http.bind = addr;
        (report, Self::launch(config, dir, mail).await)
    }

    /// Shuts down and keeps the database, the port and the mail sink, so a test can change the
    /// database or the config before [`StoppedServer::start`].
    pub async fn stop(self) -> (ShutdownReport, StoppedServer) {
        let TestServer {
            server,
            addr,
            mut config,
            mail,
            dir,
        } = self;
        let report = server.shutdown().await;
        config.http.bind = addr;
        (report, StoppedServer { config, mail, dir })
    }

    pub async fn user_id_of(&self, username: &str) -> UserId {
        self.server.user_id_of(username).await
    }

    /// Signs the Member `name` in the way a person does: a Magic Code to `<name>@lobby.test`, read
    /// from the mail, then checked.
    pub async fn sign_in(&self, name: &str) -> SignedIn {
        let email = Email::parse(&email_of(name)).expect("valid email");
        let n = self.mail.count_to(email.as_str()) + 1;
        let challenge = self.server.request_code(email.clone()).await;
        let code = self.mail.code_number(email.as_str(), n).await;
        let code = MagicCode::new(code).expect("the mail carries a code");
        match self.server.verify_code(challenge, code).await {
            VerifyOutcome::Session { session } => session,
            other => panic!("{name} is not a Member: {other:?}"),
        }
    }

    pub async fn seed_file_message(
        &self,
        author: &UserId,
        bytes: &[u8],
        name: &str,
        content: &str,
        nonce: &str,
    ) -> Attachment {
        self.server
            .seed_file_message(author, bytes, name, content, nonce)
            .await
    }

    pub async fn run_cleanup_at(&self, now: UnixMs) -> crate::jobs::CleanupReport {
        self.server.run_cleanup_at(now).await
    }

    pub fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}/api/v1/ws", self.addr)
    }

    pub async fn shutdown(self) -> ShutdownReport {
        self.server.shutdown().await
    }

    /// A server whose SFU advertises a [`MediaRelay`] in front of it, so every client's media goes
    /// through the relay, over UDP and over TCP.
    pub async fn start_behind_relay(
        names: &[&str],
        tweak: impl FnOnce(&mut Config),
    ) -> (TestServer, MediaRelay) {
        let relay = MediaRelay::bind().await;
        let (port, tcp_port) = (relay.addr().port(), relay.tcp_addr().port());
        let server = Self::start_with_members(names, |cfg| {
            cfg.sfu.advertise_port = Some(port);
            cfg.sfu.tcp_advertise_port = Some(tcp_port);
            tweak(cfg);
        })
        .await;
        relay.forward_to(server.server.sfu_addr());
        relay.forward_tcp_to(server.server.sfu_tcp_addr());
        (server, relay)
    }
}

pub struct StoppedServer {
    pub config: Config,
    mail: SmtpSink,
    dir: TempDir,
}

impl StoppedServer {
    pub async fn start(self) -> TestServer {
        TestServer::launch(self.config, self.dir, self.mail).await
    }
}

pub fn email_of(name: &str) -> String {
    format!("{}@lobby.test", name.to_lowercase())
}

/// Writes each named Member (email `<name>@lobby.test`) into the stopped server's database. The
/// first of a Lobby's first seeding is its owner.
pub async fn seed_members(config: &Config, names: &[&str]) -> Vec<Profile> {
    seed_members_at(&config.database.path, names).await
}

/// [`seed_members`] for dev tools that have no runtime of their own (`cargo xtask drive`).
pub fn seed_members_blocking(database: &Path, names: &[&str]) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(seed_members_at(database, names));
    Ok(())
}

/// The server is not running: these seeds write straight into its database.
async fn seed_members_at(database: &Path, names: &[&str]) -> Vec<Profile> {
    let (db, mut writer) = crate::db::Db::open(database)
        .await
        .expect("open the database");
    let mut profiles = Vec::new();
    for name in names {
        profiles.push(
            crate::db::users::insert(
                &mut writer,
                &Username::parse(name).expect("valid username"),
                &Email::parse(&email_of(name)).expect("valid email"),
                UnixMs::now(),
            )
            .await
            .expect("insert a Member"),
        );
    }
    if let Some(first) = profiles.first() {
        crate::db::owner::set_first(&mut writer, &first.id, UnixMs::now())
            .await
            .expect("set the owner");
    }
    close(db, writer).await;
    profiles
}

async fn close(db: crate::db::Db, writer: sqlx::SqliteConnection) {
    use sqlx::Connection as _;
    db.close().await;
    writer.close().await.expect("the seeded database closes");
}

async fn seed_history(config: &Config, author: &UserId, count: usize) {
    const SPACING_MS: i64 = 3 * 60 * 1000;
    let (db, mut writer) = crate::db::Db::open(&config.database.path)
        .await
        .expect("open the database");
    let now = UnixMs::now();
    for i in 1..=count {
        let at = UnixMs(now.0 - (count + 1 - i) as i64 * SPACING_MS);
        crate::db::messages::insert(
            &mut writer,
            author,
            &format!("Message {i} of {count}"),
            &format!("seed-{i:06}-nonce-0000"),
            &[],
            at,
        )
        .await
        .expect("insert a Message");
    }
    close(db, writer).await;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub data: String,
}

impl Mail {
    /// The first run of exactly [`MagicCode::LEN`] digits in the body: the code.
    pub fn code(&self) -> Option<String> {
        const LEN: usize = MagicCode::LEN;
        let body = self
            .data
            .split_once("\r\n\r\n")
            .map_or(&*self.data, |(_, b)| b);
        let bytes = body.as_bytes();
        (0..(bytes.len() + 1).saturating_sub(LEN)).find_map(|i| {
            let run = &bytes[i..i + LEN];
            let before = i.checked_sub(1).map(|j| bytes[j]);
            let after = bytes.get(i + LEN).copied();
            (run.iter().all(u8::is_ascii_digit)
                && !before.is_some_and(|b| b.is_ascii_digit())
                && !after.is_some_and(|b| b.is_ascii_digit()))
            .then(|| String::from_utf8_lossy(run).into_owned())
        })
    }
}

#[derive(Default)]
struct Inbox {
    mails: Mutex<Vec<Mail>>,
    arrived: Notify,
}

/// A local SMTP server that accepts everything and keeps it: plain SMTP, no AUTH, enough of RFC
/// 5321 for lettre.
pub struct SmtpSink {
    port: u16,
    inbox: Arc<Inbox>,
    _accept: AbortOnDropHandle<()>,
}

impl SmtpSink {
    pub async fn start() -> SmtpSink {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind smtp sink");
        let port = listener.local_addr().expect("sink address").port();
        let inbox = Arc::new(Inbox::default());
        let accepting = inbox.clone();
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let inbox = accepting.clone();
                tokio::spawn(async move {
                    let _ = serve_smtp(stream, &inbox).await;
                });
            }
        });
        SmtpSink {
            port,
            inbox,
            _accept: AbortOnDropHandle::new(accept),
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn mails(&self) -> Vec<Mail> {
        self.inbox.mails.lock().expect("inbox").clone()
    }

    pub fn count_to(&self, to: &str) -> usize {
        self.inbox
            .mails
            .lock()
            .expect("inbox")
            .iter()
            .filter(|m| m.to == to)
            .count()
    }

    /// Waits until more than `seen` mails went to `to`, then returns how many have and the Magic
    /// Code of the newest. Pass the count a previous call returned to get a newer code.
    pub async fn code_after(&self, to: &str, seen: usize) -> (usize, String) {
        self.code_number(to, seen + 1).await;
        let count = self.count_to(to);
        (count, self.code_number(to, count).await)
    }

    pub async fn code_number(&self, to: &str, n: usize) -> String {
        let deadline = tokio::time::Instant::now() + MAIL_WAIT_TIMEOUT;
        loop {
            let arrived = self.inbox.arrived.notified();
            let code = {
                let mails = self.inbox.mails.lock().expect("inbox");
                mails
                    .iter()
                    .filter(|m| m.to == to)
                    .nth(n - 1)
                    .map(|mail| mail.code().expect("the mail carries a code"))
            };
            if let Some(code) = code {
                return code;
            }
            if tokio::time::timeout_at(deadline, arrived).await.is_err() {
                panic!(
                    "no mail #{n} to {to} within {MAIL_WAIT_TIMEOUT:?}; inbox: {:?}",
                    self.mails()
                );
            }
        }
    }
}

async fn serve_smtp(stream: TcpStream, inbox: &Inbox) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read);
    write.write_all(b"220 lobby-test-sink ESMTP\r\n").await?;
    let mut to = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let verb = line.trim_end().to_ascii_uppercase();
        let reply: &[u8] = if verb.starts_with("EHLO") || verb.starts_with("HELO") {
            b"250-lobby-test-sink\r\n250 8BITMIME\r\n"
        } else if verb.starts_with("RCPT TO:") {
            to = line.trim_end()[8..]
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_lowercase();
            b"250 OK\r\n"
        } else if verb == "DATA" {
            write
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await?;
            let mut data = String::new();
            loop {
                line.clear();
                if lines.read_line(&mut line).await? == 0 {
                    return Ok(());
                }
                if line.trim_end() == "." {
                    break;
                }
                data.push_str(line.strip_prefix('.').unwrap_or(&line));
            }
            inbox.mails.lock().expect("inbox").push(Mail {
                to: std::mem::take(&mut to),
                data,
            });
            inbox.arrived.notify_waiters();
            b"250 OK queued\r\n"
        } else if verb == "QUIT" {
            write.write_all(b"221 Bye\r\n").await?;
            return Ok(());
        } else {
            b"250 OK\r\n"
        };
        write.write_all(reply).await?;
    }
}

const RELAY_BUFFER: usize = 2048;

/// A UDP and TCP relay on loopback between clients and the SFU. Each client gets its own
/// relay-side socket or stream towards the SFU, so the SFU still tells clients apart by source
/// address.
pub struct MediaRelay {
    addr: SocketAddr,
    tcp_addr: SocketAddr,
    shared: Arc<RelayShared>,
    _front: AbortOnDropHandle<()>,
    _tcp_front: AbortOnDropHandle<()>,
}

struct RelayShared {
    front: Arc<UdpSocket>,
    upstream: Arc<Mutex<Option<SocketAddr>>>,
    tcp_upstream: Mutex<Option<SocketAddr>>,
    blackhole: Arc<Blackhole>,
    bottlenecks: Mutex<HashMap<(SocketAddr, Direction), Arc<SharedBottleneck>>>,
    impairments: Mutex<HashMap<(SocketAddr, Direction), Arc<SharedImpairment>>>,
    /// Client address → the relay's tasks and socket towards the SFU for it.
    links: Mutex<HashMap<SocketAddr, RelayLink>>,
    tcp_links: Mutex<Vec<AbortOnDropHandle<()>>>,
}

#[derive(Default)]
struct Blackhole {
    clients: Mutex<HashSet<SocketAddr>>,
    tcp: AtomicBool,
}

impl Blackhole {
    fn drops(&self, client: SocketAddr) -> bool {
        self.clients.lock().expect("blackhole").contains(&client)
    }
}

/// Which way a datagram crosses the [`MediaRelay`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// From the SFU to the client.
    Down,
    /// From the client to the SFU.
    Up,
}

/// A bottleneck on one client's link in one [`Direction`]: datagrams queue at `bps`, and one that
/// would wait longer than `queue` is dropped, as at a congested home router.
#[derive(Debug, Clone, Copy)]
pub struct Bottleneck {
    pub bps: u64,
    pub queue: Duration,
}

type SharedBottleneck = Mutex<Option<Bottleneck>>;

/// Loss and delay on one client's link in one [`Direction`], as on a busy Wi-Fi hop: each
/// datagram is dropped `loss_percent` of the time, else held `delay` plus up to `jitter` more.
/// Datagrams keep their order, so a held one holds back those behind it. The pattern is the same
/// on every run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Impairment {
    pub loss_percent: f64,
    pub delay: Duration,
    pub jitter: Duration,
}

type SharedImpairment = Mutex<Option<Impairment>>;

/// splitmix64: the impairment's dice, seeded the same for every link.
struct Dice(u64);

impl Dice {
    /// Uniform in [0, 1).
    fn roll(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) as f64 / (u64::MAX as f64 + 1.0)
    }
}

/// Where a shaped direction ends.
enum Egress {
    Client {
        front: Arc<UdpSocket>,
        client: SocketAddr,
    },
    Sfu {
        socket: Arc<UdpSocket>,
        upstream: Arc<Mutex<Option<SocketAddr>>>,
    },
}

impl Egress {
    async fn send(&self, datagram: &[u8]) {
        match self {
            Egress::Client { front, client } => {
                let _ = front.send_to(datagram, client).await;
            }
            Egress::Sfu { socket, upstream } => {
                let upstream = *upstream.lock().expect("upstream");
                if let Some(upstream) = upstream {
                    let _ = socket.send_to(datagram, upstream).await;
                }
            }
        }
    }
}

struct RelayLink {
    /// Datagrams from the client, on their way through the uplink's shaper.
    up: mpsc::UnboundedSender<Vec<u8>>,
    _tasks: [AbortOnDropHandle<()>; 3],
}

impl MediaRelay {
    /// Forwards nothing until [`Self::forward_to`].
    pub async fn bind() -> MediaRelay {
        let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind relay"));
        let addr = front.local_addr().expect("relay address");
        let tcp_front = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tcp relay");
        let tcp_addr = tcp_front.local_addr().expect("tcp relay address");
        let shared = Arc::new(RelayShared {
            front,
            upstream: Arc::default(),
            tcp_upstream: Mutex::new(None),
            blackhole: Arc::default(),
            bottlenecks: Mutex::new(HashMap::new()),
            impairments: Mutex::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            tcp_links: Mutex::new(Vec::new()),
        });
        let front = AbortOnDropHandle::new(tokio::spawn(relay_up(shared.clone())));
        let tcp_front = AbortOnDropHandle::new(tokio::spawn(relay_tcp(tcp_front, shared.clone())));
        MediaRelay {
            addr,
            tcp_addr,
            shared,
            _front: front,
            _tcp_front: tcp_front,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn tcp_addr(&self) -> SocketAddr {
        self.tcp_addr
    }

    pub fn forward_to(&self, upstream: SocketAddr) {
        *self.shared.upstream.lock().expect("upstream") = Some(upstream);
    }

    pub fn forward_tcp_to(&self, upstream: SocketAddr) {
        *self.shared.tcp_upstream.lock().expect("tcp upstream") = Some(upstream);
    }

    /// Closes every TCP stream, and refuses new ones while `on`, as a firewall that resets them.
    pub fn blackhole_tcp(&self, on: bool) {
        self.shared.blackhole.tcp.store(on, Ordering::SeqCst);
        if on {
            self.shared.tcp_links.lock().expect("tcp links").clear();
        }
    }

    /// Drops every datagram to and from the client whose media socket is `client` while `on`.
    pub fn blackhole_client(&self, client: SocketAddr, on: bool) {
        let mut clients = self.shared.blackhole.clients.lock().expect("blackhole");
        if on {
            clients.insert(client);
        } else {
            clients.remove(&client);
        }
    }

    /// What crosses the relay in `direction` for the client whose media socket is `client` goes
    /// through `bottleneck` from now on.
    pub fn narrow_client(&self, client: SocketAddr, direction: Direction, bottleneck: Bottleneck) {
        *self
            .shared
            .bottleneck(client, direction)
            .lock()
            .expect("bottleneck") = Some(bottleneck);
    }

    /// What crosses the relay in `direction` for the client whose media socket is `client` is
    /// lost and held as `impairment` says from now on.
    pub fn impair_client(&self, client: SocketAddr, direction: Direction, impairment: Impairment) {
        *self
            .shared
            .impairment(client, direction)
            .lock()
            .expect("impairment") = Some(impairment);
    }
}

async fn relay_up(shared: Arc<RelayShared>) {
    let mut buf = vec![0u8; RELAY_BUFFER];
    loop {
        let Ok((n, client)) = shared.front.recv_from(&mut buf).await else {
            continue;
        };
        let forwarding = shared.upstream.lock().expect("upstream").is_some();
        if !forwarding || shared.blackhole.drops(client) {
            continue;
        }
        let _ = shared.link_for(client).send(buf[..n].to_vec());
    }
}

async fn relay_tcp(front: TcpListener, shared: Arc<RelayShared>) {
    loop {
        let Ok((mut client, _)) = front.accept().await else {
            continue;
        };
        let upstream = *shared.tcp_upstream.lock().expect("tcp upstream");
        let Some(upstream) = upstream.filter(|_| !shared.blackhole.tcp.load(Ordering::SeqCst))
        else {
            continue;
        };
        let link = tokio::spawn(async move {
            if let Ok(mut sfu) = TcpStream::connect(upstream).await {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut sfu).await;
            }
        });
        let mut links = shared.tcp_links.lock().expect("tcp links");
        links.retain(|l| !l.is_finished());
        links.push(AbortOnDropHandle::new(link));
    }
}

impl RelayShared {
    fn bottleneck(&self, client: SocketAddr, direction: Direction) -> Arc<SharedBottleneck> {
        self.bottlenecks
            .lock()
            .expect("bottlenecks")
            .entry((client, direction))
            .or_default()
            .clone()
    }

    fn impairment(&self, client: SocketAddr, direction: Direction) -> Arc<SharedImpairment> {
        self.impairments
            .lock()
            .expect("impairments")
            .entry((client, direction))
            .or_default()
            .clone()
    }

    /// The sender of the client's uplink, which starts the client's link tasks on first use.
    fn link_for(&self, client: SocketAddr) -> mpsc::UnboundedSender<Vec<u8>> {
        let mut links = self.links.lock().expect("links");
        let link = links.entry(client).or_insert_with(|| {
            let bound = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind relay link");
            bound.set_nonblocking(true).expect("nonblocking relay link");
            let socket = Arc::new(UdpSocket::from_std(bound).expect("relay link"));
            let (up, up_arrivals) = mpsc::unbounded_channel();
            let (down, down_arrivals) = mpsc::unbounded_channel();
            let up_shaper = tokio::spawn(shape(
                up_arrivals,
                self.bottleneck(client, Direction::Up),
                self.impairment(client, Direction::Up),
                Egress::Sfu {
                    socket: socket.clone(),
                    upstream: self.upstream.clone(),
                },
            ));
            let down_shaper = tokio::spawn(shape(
                down_arrivals,
                self.bottleneck(client, Direction::Down),
                self.impairment(client, Direction::Down),
                Egress::Client {
                    front: self.front.clone(),
                    client,
                },
            ));
            let down_reader =
                tokio::spawn(relay_down(socket, client, self.blackhole.clone(), down));
            RelayLink {
                up,
                _tasks: [
                    AbortOnDropHandle::new(up_shaper),
                    AbortOnDropHandle::new(down_shaper),
                    AbortOnDropHandle::new(down_reader),
                ],
            }
        });
        link.up.clone()
    }
}

/// Reads what the SFU sends a client.
async fn relay_down(
    from: Arc<UdpSocket>,
    client: SocketAddr,
    blackhole: Arc<Blackhole>,
    shaper: mpsc::UnboundedSender<Vec<u8>>,
) {
    let mut buf = vec![0u8; RELAY_BUFFER];
    loop {
        let Ok((n, _)) = from.recv_from(&mut buf).await else {
            continue;
        };
        if blackhole.drops(client) {
            continue;
        }
        let _ = shaper.send(buf[..n].to_vec());
    }
}

/// Carries one direction of one client's link to `egress` through the wire's bottleneck and
/// impairment.
async fn shape(
    mut arrivals: mpsc::UnboundedReceiver<Vec<u8>>,
    bottleneck: Arc<SharedBottleneck>,
    impairment: Arc<SharedImpairment>,
    egress: Egress,
) {
    let mut queued: VecDeque<(tokio::time::Instant, Vec<u8>)> = VecDeque::new();
    let mut link_free = tokio::time::Instant::now();
    let mut dice = Dice(0);
    loop {
        let due = queued.front().map(|(at, _)| *at);
        let next_due = async {
            match due {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            arrived = arrivals.recv() => {
                let Some(datagram) = arrived else {
                    return;
                };
                let narrowed = *bottleneck.lock().expect("bottleneck");
                let impaired = *impairment.lock().expect("impairment");
                let held = match impaired {
                    Some(i) if dice.roll() * 100.0 < i.loss_percent => continue,
                    Some(i) => Some(i.delay + i.jitter.mul_f64(dice.roll())),
                    None => None,
                };
                let now = tokio::time::Instant::now();
                let due = match narrowed {
                    Some(Bottleneck { bps, queue }) => {
                        let start = link_free.max(now);
                        if start - now > queue {
                            continue;
                        }
                        link_free = start
                            + Duration::from_secs_f64(datagram.len() as f64 * 8.0 / bps as f64);
                        link_free
                    }
                    None if held.is_none() => {
                        egress.send(&datagram).await;
                        continue;
                    }
                    None => now,
                };
                let due = (due + held.unwrap_or_default())
                    .max(queued.back().map_or(due, |(at, _)| *at));
                queued.push_back((due, datagram));
            }
            () = next_due => {
                if let Some((_, datagram)) = queued.pop_front() {
                    egress.send(&datagram).await;
                }
            }
        }
    }
}
