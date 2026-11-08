//! Who may come in: Magic Codes, the first Member of an empty Lobby (bootstrap), Ask to Join and
//! Waiting for a Vouch. [`standing`] decides where an email stands, for verify and join alike. Join
//! decides again in the transaction that spends its ticket, and any refusal rolls it back, so the
//! ticket stays usable.

use std::time::Duration;

use lobby_protocol::{
    AccessRequestId, Email, JoinOutcome, JoinStatus, JoinTicket, MagicCode, RequestLabel, SignedIn,
    UnixMs, Username, VerifyOutcome, WaitToken,
};
use sqlx::SqliteConnection;
use tokio::time::Instant;
use tracing::{debug, info};

use super::{Domain, WriteError, Writer};
use crate::{
    auth::tokens::{self, Digest, Minted},
    client_ip::ClientIp,
    config::AuthConfig,
    db::{
        self, DbError,
        admission::{self, PendingCounts, WaitState},
        owner, sessions,
        users::{self, Profile},
    },
    limits::{CodeSend, Recipient},
};

const TICKET_TTL: Duration = Duration::from_secs(10 * 60);
/// How long a wait token checks on a request to join. After that a new code shows where the request
/// stands and hands out a new one.
const WAIT_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

/// A `POST /auth/join`, its session minted beforehand.
pub struct Join {
    pub ticket_hash: Digest,
    pub client: ClientIp,
    pub username: Username,
    pub minted: Minted,
}

/// A request for a Magic Code, its challenge minted beforehand.
pub struct CodeRequest {
    pub email: Email,
    pub client: ClientIp,
    pub challenge_hash: Digest,
    /// Printed in the mail, so a Member can tell which one answers their request.
    pub label: RequestLabel,
}

/// A challenge's Magic Code to check, its answers minted beforehand; at most one of them is
/// stored.
pub struct Verify {
    pub challenge_hash: Digest,
    pub code: MagicCode,
    pub minted: Minted,
    pub ticket: JoinTicket,
    pub ticket_hash: Digest,
}

/// Where an email stands at the front door.
#[derive(Debug)]
enum Standing {
    Member(Profile),
    /// Its Ask to Join waits for a Vouch, or was declined and the Decline still holds.
    Waiting(AccessRequestId),
    /// The bootstrap email of a Lobby that has never had a Member.
    CreateLobby,
    AskToJoin,
}

/// In the caller's transaction. A Lobby that has never had a Member refuses every email but its
/// bootstrap one, and a locked Lobby every newcomer.
async fn standing(
    conn: &mut SqliteConnection,
    auth: &AuthConfig,
    email: &Email,
) -> Result<Standing, WriteError> {
    if let Some(profile) = users::active_by_email(conn, email).await? {
        return Ok(Standing::Member(profile));
    }
    if let Some(request) = admission::pending_for_email(conn, email).await? {
        return Ok(Standing::Waiting(request));
    }
    // A Decline holds until the cleanup job drops its row: the asker who asks again follows it
    // with a new wait token and learns the Decline the way the first asker did. The email was
    // verified by code, so this tells nobody else anything.
    if let Some(request) = admission::declined_for_email(conn, email).await? {
        return Ok(Standing::Waiting(request));
    }
    if !users::any_user(conn).await? {
        return if is_bootstrap(&auth.bootstrap_email, email) {
            Ok(Standing::CreateLobby)
        } else {
            Err(WriteError::NotSetUp)
        };
    }
    if !auth.join_requests_enabled {
        return Err(WriteError::Locked);
    }
    Ok(Standing::AskToJoin)
}

/// Asks to Join that may wait for a Vouch at once. Each one rings every Member's bell, so a
/// stranger with many addresses cannot flood them.
const MAX_PENDING_REQUESTS: i64 = 20;
/// Of those, from one client site (IPv4 address or IPv6 /48), and for one mailbox (`+tag` aliases
/// are one). Without them one stranger fills every place and no newcomer can ask. Five lets a
/// household onboard together.
pub const MAX_PENDING_PER_SITE: i64 = 5;
const MAX_PENDING_PER_MAILBOX: i64 = 2;

impl PendingCounts {
    fn full(&self) -> bool {
        self.total >= MAX_PENDING_REQUESTS
            || self.from_site >= MAX_PENDING_PER_SITE
            || self.for_mailbox >= MAX_PENDING_PER_MAILBOX
    }
}

fn is_bootstrap(configured: &str, email: &Email) -> bool {
    Email::parse(configured).is_ok_and(|b| &b == email)
}

fn new_code() -> MagicCode {
    let n: u32 = rand::random_range(0..100_000_000);
    MagicCode::new(format!("{n:08}")).expect("eight digits")
}

/// One more device waiting on `request`: devices asking with one email share the request.
async fn new_wait(
    conn: &mut SqliteConnection,
    request: &AccessRequestId,
    now: UnixMs,
) -> Result<WaitToken, DbError> {
    let (wait_token, wait_hash) = tokens::wait_token();
    let expires_at = now.saturating_add(WAIT_TTL);
    admission::insert_wait(conn, request, &wait_hash, expires_at, now).await?;
    Ok(wait_token)
}

impl Domain {
    /// A new code for the request's challenge, mailed once it is stored unless
    /// [`CodeSend::Withhold`]. Over the client's own send caps nothing is stored and the asker
    /// hears when to try again.
    pub async fn issue_code(&self, r: CodeRequest) -> Result<(), WriteError> {
        self.run_at_front_door(move |w| {
            Box::pin(async move {
                let CodeRequest {
                    email,
                    client,
                    challenge_hash,
                    label,
                } = r;
                let instant = Instant::now();
                let recipient = if users::active_by_email(&mut w.conn, &email).await?.is_some()
                    || (is_bootstrap(&w.cfg.auth.bootstrap_email, &email)
                        && !users::any_user(&mut w.conn).await?)
                {
                    Recipient::Member
                } else {
                    Recipient::Stranger
                };
                let send = match w.code_sends.try_acquire(&email, client, recipient, instant) {
                    Ok(send) => send,
                    Err(retry_at) => {
                        info!(
                            domain = email.domain(),
                            "magic code not sent: send cap reached"
                        );
                        let wait = retry_at.saturating_duration_since(instant);
                        return Err(WriteError::CodesCapped(wait));
                    }
                };
                let now = UnixMs::now();
                let code = new_code();
                let hash = tokens::code_digest(&email, code.expose());
                let expires_at = now.saturating_add(w.cfg.auth.magic_code_ttl);
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                admission::insert_code(&mut tx, &challenge_hash, &email, &hash, expires_at, now)
                    .await?;
                tx.commit().await?;
                match send {
                    CodeSend::Mail => {
                        // The mailer sends on its own task: the writer does not wait for SMTP.
                        w.mailer.send_code(&email, &code, &label);
                        info!(domain = email.domain(), "magic code issued");
                    }
                    CodeSend::Withhold => info!(
                        domain = email.domain(),
                        "magic code not mailed: a shared send cap is reached"
                    ),
                }
                Ok(())
            })
        })
        .await
    }

    /// Checks the challenge's code, then answers by its email's standing: a session for a Member,
    /// a wait token for a pending Ask to Join, or a ticket to choose a name with.
    pub async fn verify_code(&self, v: Verify) -> Result<VerifyOutcome, WriteError> {
        self.run_at_front_door(move |w| Box::pin(verify(w, v)))
            .await
    }
}

async fn verify(w: &mut Writer, v: Verify) -> Result<VerifyOutcome, WriteError> {
    let Verify {
        challenge_hash,
        code,
        minted,
        ticket,
        ticket_hash,
    } = v;
    let now = UnixMs::now();
    let mut tx = db::begin_immediate(&mut w.conn).await?;
    let email = match check_code(&mut tx, &challenge_hash, &code, now).await {
        Ok(email) => email,
        Err(e) => {
            // A counted attempt sticks.
            tx.commit().await?;
            debug!(error = %e, "magic code refused");
            return Err(e);
        }
    };
    let ticket_expires = now.saturating_add(TICKET_TTL);
    let outcome = match standing(&mut tx, &w.cfg.auth, &email).await {
        Err(e) => {
            // The code was right and is spent either way.
            tx.commit().await?;
            info!(domain = email.domain(), error = %e, "verify: no way in");
            return Err(e);
        }
        Ok(Standing::Member(profile)) => {
            let session = sessions::create(&mut tx, &profile.id, &minted.pair(), now).await?;
            tx.commit().await?;
            info!(user = %profile.id, session = session.0, "signed in with a magic code");
            return Ok(VerifyOutcome::Session {
                session: SignedIn {
                    user: profile.user(),
                    tokens: minted.into_tokens(),
                },
            });
        }
        Ok(Standing::Waiting(request)) => VerifyOutcome::PendingApproval {
            wait_token: new_wait(&mut tx, &request, now).await?,
        },
        Ok(Standing::CreateLobby) => {
            admission::insert_ticket(&mut tx, &email, &ticket_hash, ticket_expires, now).await?;
            VerifyOutcome::CreateLobby { ticket }
        }
        Ok(Standing::AskToJoin) => {
            admission::insert_ticket(&mut tx, &email, &ticket_hash, ticket_expires, now).await?;
            VerifyOutcome::AskToJoin { ticket }
        }
    };
    tx.commit().await?;
    debug!(domain = email.domain(), ?outcome, "magic code verified");
    Ok(outcome)
}

/// Refused unless `username` is free for `email`.
async fn name_free(
    conn: &mut SqliteConnection,
    username: &Username,
    email: &Email,
) -> Result<(), WriteError> {
    match users::taken(conn, username, email, None).await? {
        Some(taken) => Err(taken.into()),
        None => Ok(()),
    }
}

impl Domain {
    /// Spends the ticket by the email's standing now: the first Member of an empty Lobby, who owns
    /// it and is signed in at once, or an Ask to Join, answered with a wait token.
    pub async fn join(&self, j: Join) -> Result<JoinOutcome, WriteError> {
        self.run_at_front_door(move |w| {
            Box::pin(async move {
                let Join {
                    ticket_hash,
                    client,
                    username,
                    minted,
                } = j;
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                let email = admission::consume_ticket(&mut tx, &ticket_hash, now).await??;
                match standing(&mut tx, &w.cfg.auth, &email).await? {
                    Standing::Member(_) => Err(WriteError::EmailTaken),
                    Standing::Waiting(request) => {
                        let wait_token = new_wait(&mut tx, &request, now).await?;
                        tx.commit().await?;
                        info!(request = %request.as_str(), domain = email.domain(),
                            "join: the request already waits");
                        Ok(JoinOutcome::PendingApproval { wait_token })
                    }
                    Standing::CreateLobby => {
                        name_free(&mut tx, &username, &email).await?;
                        let profile = users::insert(&mut tx, &username, &email, now).await?;
                        owner::set_first(&mut tx, &profile.id, now).await?;
                        let session =
                            sessions::create(&mut tx, &profile.id, &minted.pair(), now).await?;
                        tx.commit().await?;
                        w.lobby.bootstrapped(profile.clone());
                        info!(user = %profile.id, session = session.0,
                            "the first Member joined (bootstrap)");
                        Ok(JoinOutcome::Session {
                            session: SignedIn {
                                user: profile.user(),
                                tokens: minted.into_tokens(),
                            },
                        })
                    }
                    Standing::AskToJoin => {
                        name_free(&mut tx, &username, &email).await?;
                        let site = client.site_key().to_string();
                        let waiting = admission::pending_counts(&mut tx, &site, &email).await?;
                        if waiting.full() {
                            info!(
                                domain = email.domain(),
                                "join refused: too many requests wait"
                            );
                            return Err(WriteError::TooManyRequests);
                        }
                        let request =
                            admission::insert_request(&mut tx, &username, &email, &site, now)
                                .await?;
                        let wait_token = new_wait(&mut tx, &request.id, now).await?;
                        tx.commit().await?;
                        info!(request = %request.id.as_str(), domain = email.domain(),
                            "access requested");
                        w.lobby.access_request_upserted(request);
                        Ok(JoinOutcome::PendingApproval { wait_token })
                    }
                }
            })
        })
        .await
    }

    /// Spends the wait token of a request that was Vouched in and signs this device in.
    pub async fn sign_in_after_vouch(
        &self,
        wait_hash: Digest,
        minted: Minted,
    ) -> Result<JoinStatus, WriteError> {
        self.run_at_front_door(move |w| {
            Box::pin(async move {
                let now = UnixMs::now();
                let mut tx = db::begin_immediate(&mut w.conn).await?;
                // Read again under the writer: another check may have spent it meanwhile.
                let Some(WaitState::VouchedIn { wait, user }) =
                    admission::wait_state(&mut tx, &wait_hash, now).await?
                else {
                    return Err(WriteError::InvalidWait);
                };
                let Some(profile) = users::active_profile(&mut tx, &user).await? else {
                    return Err(WriteError::InvalidWait);
                };
                admission::use_wait(&mut tx, wait, now).await?;
                let session = sessions::create(&mut tx, &profile.id, &minted.pair(), now).await?;
                tx.commit().await?;
                info!(user = %profile.id, session = session.0, "signed in after a Vouch");
                Ok(JoinStatus::Session {
                    session: SignedIn {
                        user: profile.user(),
                        tokens: minted.into_tokens(),
                    },
                })
            })
        })
        .await
    }
}

/// In the caller's transaction: the challenge's unused code, its attempt counted before the
/// constant-time compare, expiry only after a match (so only a correct code reveals it), then the
/// conditional mark-used. The caller commits either way, so a counted attempt sticks. A challenge
/// with no code (over the send caps, or never issued) reads as a wrong code.
async fn check_code(
    conn: &mut SqliteConnection,
    challenge_hash: &Digest,
    code: &MagicCode,
    now: UnixMs,
) -> Result<Email, WriteError> {
    let Some(row) = admission::challenged_code(conn, challenge_hash).await? else {
        return Err(WriteError::NoLiveCode);
    };
    if !admission::count_attempt(conn, row.id).await? {
        return Err(WriteError::TooManyAttempts);
    }
    let typed = tokens::code_digest(&row.email, code.expose());
    if !tokens::same_digest(&row.code_hash, &typed) {
        return Err(WriteError::InvalidCode);
    }
    if row.expires_at <= now {
        return Err(WriteError::CodeExpired);
    }
    admission::use_code(conn, row.id, now).await?;
    Ok(row.email)
}
