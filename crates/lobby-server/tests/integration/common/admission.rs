//! Admission over HTTP: Magic Codes read from the server's mail, verify, a name for Create Lobby
//! or Ask to Join, and Waiting for a Vouch.

use lobby_server::testing::{TestServer, email_of};
use serde_json::{Value, json};

use super::*;

/// A request's challenge, and the code mailed with it.
pub struct Code {
    pub challenge: Asked,
    pub code: String,
}

/// The challenge of a `POST /auth/magic-code` answer.
pub struct Asked(pub String);

pub fn asked(res: &HttpResponse) -> Asked {
    assert_eq!(res.status, 200, "{}", res.body);
    let challenge = res.json()["challenge"].as_str().unwrap().to_owned();
    assert_eq!(challenge.len(), 43, "32 bytes, base64url");
    Asked(challenge)
}

pub async fn request_code(env: &TestServer, email: &str) -> Code {
    let n = env.mail.count_to(email) + 1;
    let res = post_json(
        env.addr,
        "/api/v1/auth/magic-code",
        &json!({"email": email}),
    )
    .await;
    Code {
        challenge: asked(&res),
        code: env.mail.code_number(email, n).await,
    }
}

pub async fn verify(env: &TestServer, asked: &Asked, code: &str) -> HttpResponse {
    post_json(
        env.addr,
        "/api/v1/auth/magic-code/verify",
        &json!({"challenge": asked.0, "code": code}),
    )
    .await
}

/// A device behind the trusted local proxy, which forwards its address.
pub struct Device<'a> {
    pub ip: &'a str,
}

impl Device<'_> {
    async fn post(&self, env: &TestServer, path: &str, body: Value) -> HttpResponse {
        http(
            env.addr,
            "POST",
            path,
            &[
                ("Content-Type", "application/json"),
                ("X-Forwarded-For", self.ip),
            ],
            &body.to_string(),
        )
        .await
    }

    pub async fn request_code(&self, env: &TestServer, email: &str) -> HttpResponse {
        self.post(env, "/api/v1/auth/magic-code", json!({"email": email}))
            .await
    }

    pub async fn ask(&self, env: &TestServer, email: &str) -> Asked {
        asked(&self.request_code(env, email).await)
    }

    pub async fn join(&self, env: &TestServer, ticket: &str, username: &str) -> HttpResponse {
        let body = json!({"ticket": ticket, "username": username});
        self.post(env, "/api/v1/auth/join", body).await
    }

    pub async fn verify(&self, env: &TestServer, asked: &Asked, code: &str) -> HttpResponse {
        let body = json!({"challenge": asked.0, "code": code});
        self.post(env, "/api/v1/auth/magic-code/verify", body).await
    }
}

pub fn wrong_code(right: &str) -> String {
    let first = if right.starts_with('0') { '1' } else { '0' };
    std::iter::repeat_n(first, right.len()).collect()
}

pub async fn verified(env: &TestServer, email: &str) -> Value {
    let code = request_code(env, email).await;
    let res = verify(env, &code.challenge, &code.code).await;
    assert_eq!(res.status, 200, "{}", res.body);
    res.json()
}

/// The ticket of a verify that answers `next` (`create_lobby` or `ask_to_join`).
pub async fn ticket(env: &TestServer, email: &str, next: &str) -> String {
    let outcome = verified(env, email).await;
    assert_eq!(outcome["next"], next, "{outcome}");
    let ticket = outcome["ticket"].as_str().unwrap().to_owned();
    assert_eq!(ticket.len(), 43, "32 bytes, base64url");
    ticket
}

pub async fn join(env: &TestServer, ticket: &str, username: &str) -> HttpResponse {
    post_json(
        env.addr,
        "/api/v1/auth/join",
        &json!({"ticket": ticket, "username": username}),
    )
    .await
}

pub async fn post_auth(env: &TestServer, path: &str, token: &str, body: &Value) -> HttpResponse {
    let bearer = format!("Bearer {token}");
    http(
        env.addr,
        "POST",
        path,
        &[
            ("Content-Type", "application/json"),
            ("Authorization", bearer.as_str()),
        ],
        &body.to_string(),
    )
    .await
}

pub fn assert_message(res: &HttpResponse, status: u16, code: &str, message: &str) {
    assert_envelope(res, status, code);
    assert_eq!(res.json()["error"]["message"], message, "{}", res.body);
}

/// The wait token of a `pending_approval` answer.
pub fn wait_token(outcome: &Value) -> String {
    assert_eq!(outcome["next"], "pending_approval", "{outcome}");
    let token = outcome["waitToken"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 43, "32 bytes, base64url");
    token
}

/// An Ask to Join for `name` (email `<name>@lobby.test`); returns the pending request's id as
/// the given Member's socket sees it, and the asker's wait token.
pub async fn ask_to_join(env: &TestServer, name: &str, watcher: &mut Ws) -> (String, String) {
    let email = email_of(name);
    let ticket = ticket(env, &email, "ask_to_join").await;
    let res = join(env, &ticket, name).await;
    request_seen(&res, &email, name, watcher).await
}

/// [`ask_to_join`] for `email`, from `device`.
pub async fn ask_to_join_from(
    env: &TestServer,
    device: &Device<'_>,
    email: &str,
    name: &str,
    watcher: &mut Ws,
) -> (String, String) {
    let ticket = ticket(env, email, "ask_to_join").await;
    let res = device.join(env, &ticket, name).await;
    request_seen(&res, email, name, watcher).await
}

async fn request_seen(
    res: &HttpResponse,
    email: &str,
    name: &str,
    watcher: &mut Ws,
) -> (String, String) {
    assert_eq!(res.status, 200, "{}", res.body);
    let wait = wait_token(&res.json());
    let upsert = next_of(watcher, "ACCESS_REQUEST_UPSERT").await;
    assert_eq!(upsert["request"]["username"], name);
    assert_eq!(upsert["request"]["requesterLabel"], email);
    (upsert["request"]["id"].as_str().unwrap().to_owned(), wait)
}

pub async fn join_status(env: &TestServer, wait: &str) -> HttpResponse {
    post_json(
        env.addr,
        "/api/v1/auth/join/status",
        &json!({"waitToken": wait}),
    )
    .await
}
