//! Magic Codes, admission (bootstrap, Ask to Join, Vouch, Decline, Waiting for a Vouch), join
//! tickets, Sign Out and the per-IP limits, over real loopback sockets with every mail caught by
//! the `SmtpSink`.

use std::time::Duration;

use crate::common::{admission::*, *};
use lobby_protocol::UnixMs;
use lobby_server::{
    Config,
    testing::{MAX_PENDING_PER_SITE, PENDING_MAX_AGE, TestServer, email_of},
};
use serde_json::json;

const ADMIN: &str = "admin@lobby.test";

/// The bootstrap email names itself and owns the Lobby; its next code signs it in, and its READY
/// carries its email. Anyone else hears "not set up" right after their code, until then.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bootstrap_email_sets_the_lobby_up_and_its_next_code_signs_it_in() {
    let env = TestServer::start_with(|c| c.auth.bootstrap_email = ADMIN.into()).await;

    assert_eq!(get(env.addr, "/api/v1/lobby").await.json()["setUp"], false);
    let code = request_code(&env, "early@lobby.test").await;
    let refused = verify(&env, &code.challenge, &code.code).await;
    assert_message(
        &refused,
        403,
        "FORBIDDEN",
        "This Lobby has not been set up yet",
    );
    assert_eq!(refused.json()["error"]["detail"], "NOT_SET_UP");

    let admin = ticket(&env, ADMIN, "create_lobby").await;
    let res = join(&env, &admin, "Admin").await;
    assert_eq!(res.status, 200, "{}", res.body);
    let joined = res.json();
    assert_eq!(joined["next"], "session");
    assert_eq!(joined["session"]["user"]["username"], "Admin");
    let (mut ws, ready) = identify(&env.ws_url(), &access(&joined["session"])).await;
    assert_eq!(ready["members"].as_array().unwrap().len(), 1);
    assert_eq!(ready["lobby"]["setUp"], true);
    assert_eq!(ready["lobby"]["owner"], joined["session"]["user"]["id"]);
    assert_eq!(ready["email"], ADMIN);
    assert_eq!(get(env.addr, "/api/v1/lobby").await.json()["setUp"], true);

    let again = verified(&env, ADMIN).await;
    assert_eq!(again["next"], "session", "{again}");
    assert_eq!(
        again["session"]["user"]["id"],
        joined["session"]["user"]["id"]
    );
    let tokens = &again["session"]["tokens"];
    assert_eq!(tokens["accessToken"].as_str().unwrap().len(), 43);
    let access_ttl = tokens["accessExpiresAt"].as_i64().unwrap() - UnixMs::now().0;
    assert!((890_000..=900_000).contains(&access_ttl), "{access_ttl}");

    let early = ticket(&env, "early@lobby.test", "ask_to_join").await;
    wait_token(&join(&env, &early, "early").await.json());
    let upsert = next_of(&mut ws, "ACCESS_REQUEST_UPSERT").await;
    assert_eq!(upsert["request"]["username"], "early");
    assert!(env.shutdown().await.is_clean());
}

/// Two devices ask with one email: they share the one request. A Vouch makes the new Member,
/// whose next code signs them in; a Decline holds for an asker who asks again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_to_join_vouch_and_decline_reach_every_member_in_order() {
    let env = TestServer::start_with_members(&["alice", "bob", "eve"], |_| {}).await;
    let (mut a, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    let (mut b, _) = identify(&env.ws_url(), &access(&sign_in(&env, "bob").await)).await;

    let other_device = ticket(&env, &email_of("carol"), "ask_to_join").await;
    let (carol, _) = ask_to_join(&env, "carol", &mut a).await;
    let (dave, _) = ask_to_join(&env, "dave", &mut a).await;
    wait_token(&join(&env, &other_device, "carol_two").await.json());
    wait_token(&verified(&env, &email_of("carol")).await);
    for _ in 0..2 {
        next_of(&mut b, "ACCESS_REQUEST_UPSERT").await;
    }
    let (_late, ready) = identify(&env.ws_url(), &access(&sign_in(&env, "eve").await)).await;
    let listed: Vec<&str> = ready["accessRequests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["username"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["dave", "carol"]);

    let alice = access(&sign_in(&env, "alice").await);
    let res = post_auth(
        &env,
        &format!("/api/v1/access-requests/{carol}/vouch"),
        &alice,
        &json!({}),
    )
    .await;
    assert_eq!(res.status, 204, "{}", res.body);
    for ws in [&mut a, &mut b] {
        let removed = next_of(ws, "ACCESS_REQUEST_REMOVED").await;
        assert_eq!(removed["id"], carol.as_str());
        assert_eq!(removed["outcome"], "vouched_in");
        assert!(removed["handledBy"].as_str().unwrap().starts_with("usr_"));
        let member = next_json(ws).await;
        assert_eq!(member["t"], "MEMBER_UPSERT", "{member}");
        assert_eq!(member["d"]["member"]["username"], "carol");
        assert_eq!(member["d"]["member"]["presence"], "offline");
    }
    let session = verified(&env, &email_of("carol")).await;
    assert_eq!(session["next"], "session", "{session}");
    assert_eq!(session["session"]["user"]["username"], "carol");
    assert_message(
        &post_auth(
            &env,
            &format!("/api/v1/access-requests/{carol}/vouch"),
            &alice,
            &json!({}),
        )
        .await,
        404,
        "NOT_FOUND",
        "Access request not found",
    );

    let bob = access(&sign_in(&env, "bob").await);
    let res = post_auth(
        &env,
        &format!("/api/v1/access-requests/{dave}/decline"),
        &bob,
        &json!({}),
    )
    .await;
    assert_eq!(res.status, 204, "{}", res.body);
    let removed = next_of(&mut a, "ACCESS_REQUEST_REMOVED").await;
    assert_eq!(removed["outcome"], "declined");
    assert_message(
        &post_auth(
            &env,
            &format!("/api/v1/access-requests/{dave}/decline"),
            &bob,
            &json!({}),
        )
        .await,
        404,
        "NOT_FOUND",
        "Access request not found",
    );
    // A Decline holds: asking again with a fresh code ends in the same Decline, and no new
    // request reaches the Members.
    let again = verified(&env, &email_of("dave")).await;
    assert_eq!(
        join_status(&env, &wait_token(&again)).await.json(),
        json!({"next": "declined"})
    );
    // A malformed id is a 400; a request without a token spends nothing and is a 401.
    assert_envelope(
        &post_auth(&env, "/api/v1/access-requests/nope/vouch", &bob, &json!({})).await,
        400,
        "INVALID_REQUEST",
    );
    assert_envelope(
        &post_json(
            env.addr,
            &format!("/api/v1/access-requests/{dave}/vouch"),
            &json!({}),
        )
        .await,
        401,
        "AUTH_FAILED",
    );
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_so_many_requests_wait_for_a_vouch_at_once() {
    let env = TestServer::start_with_members(&["alice"], behind_proxy).await;
    let (mut a, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    let sites = (0..=20)
        .map(|n| format!("198.51.100.{n}"))
        .collect::<Vec<_>>();
    let mut first = String::new();
    for (n, site) in sites[..20].iter().enumerate() {
        let (name, device) = (format!("guest{n}"), Device { ip: site });
        let (id, _) = ask_to_join_from(&env, &device, &email_of(&name), &name, &mut a).await;
        if n == 0 {
            first = id;
        }
    }
    let (late, device) = (
        ticket(&env, &email_of("late"), "ask_to_join").await,
        Device { ip: &sites[20] },
    );
    assert_message(
        &device.join(&env, &late, "late").await,
        503,
        "LIMIT_REACHED",
        "Too many people are waiting to join. Try again later.",
    );
    // The refusal rolled back, so the ticket is unspent, and a place frees up when one is handled.
    let alice = access(&sign_in(&env, "alice").await);
    let path = format!("/api/v1/access-requests/{first}/decline");
    assert_eq!(post_auth(&env, &path, &alice, &json!({})).await.status, 204);
    assert_eq!(
        device.join(&env, &late, "late").await.status,
        200,
        "a place is free"
    );
    assert!(env.shutdown().await.is_clean());
}

/// One site and one mailbox hold a few places each, not all of them, and a request that waits too
/// long is dropped, so a stranger who stops asking does not hold a place for good.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_site_or_mailbox_cannot_fill_the_places_and_stale_requests_expire() {
    let env = TestServer::start_with_members(&["alice"], behind_proxy).await;
    let (mut a, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    let (site, other, third) = (
        Device { ip: "203.0.113.9" },
        Device { ip: "203.0.113.10" },
        Device { ip: "203.0.113.11" },
    );
    let mut waits = Vec::new();
    for n in 0..MAX_PENDING_PER_SITE {
        let name = format!("guest{n}");
        let (_, wait) = ask_to_join_from(&env, &site, &email_of(&name), &name, &mut a).await;
        waits.push(wait);
    }
    let late = ticket(&env, &email_of("late"), "ask_to_join").await;
    let full = "Too many people are waiting to join. Try again later.";
    assert_message(
        &site.join(&env, &late, "late").await,
        503,
        "LIMIT_REACHED",
        full,
    );
    // Another site is not held back, and the refused ticket was not spent.
    assert_eq!(other.join(&env, &late, "late").await.status, 200);
    next_of(&mut a, "ACCESS_REQUEST_UPSERT").await;

    for (n, email) in ["box+one@lobby.test", "box+two@lobby.test"]
        .into_iter()
        .enumerate()
    {
        ask_to_join_from(&env, &third, email, &format!("box{n}"), &mut a).await;
    }
    let tagged = ticket(&env, "box+three@lobby.test", "ask_to_join").await;
    assert_message(
        &third.join(&env, &tagged, "box2").await,
        503,
        "LIMIT_REACHED",
        full,
    );

    let waiting_for = Duration::from_secs(60);
    let kept = UnixMs::now().saturating_add(PENDING_MAX_AGE - waiting_for);
    assert_eq!(env.run_cleanup_at(kept).await.stale_requests, 0);
    let stale = UnixMs::now().saturating_add(PENDING_MAX_AGE + waiting_for);
    let waiting = MAX_PENDING_PER_SITE as u64 + 3;
    assert_eq!(env.run_cleanup_at(stale).await.stale_requests, waiting);
    for _ in 0..waiting {
        next_of(&mut a, "ACCESS_REQUEST_REMOVED").await;
    }
    assert_envelope(&join_status(&env, &waits[0]).await, 401, "AUTH_FAILED");
    ask_to_join_from(&env, &site, &email_of("again"), "again", &mut a).await;
    assert!(env.shutdown().await.is_clean());
}

/// A ticket survives a refused name (the refusal rolls back), works once, lasts ten minutes, and
/// is refused once its email became a Member's meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ticket_works_once_for_ten_minutes_and_survives_a_refused_name() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let carol = ticket(&env, &email_of("carol"), "ask_to_join").await;
    let taken = join(&env, &carol, "ALICE").await;
    assert_message(&taken, 409, "CONFLICT", "Username already taken");
    assert_eq!(taken.json()["error"]["detail"], "USERNAME_TAKEN");
    wait_token(&join(&env, &carol, "carol").await.json());
    assert_message(
        &join(&env, &carol, "carol").await,
        401,
        "AUTH_FAILED",
        "Invalid or used join ticket",
    );

    let dave = ticket(&env, &email_of("dave"), "ask_to_join").await;
    sqlx::query("UPDATE join_tickets SET expires_at = 1 WHERE used_at IS NULL")
        .execute(&mut sqlite(&env).await)
        .await
        .unwrap();
    assert_message(
        &join(&env, &dave, "dave").await,
        401,
        "AUTH_EXPIRED",
        "Join ticket has expired",
    );

    let erin = ticket(&env, &email_of("erin"), "ask_to_join").await;
    let alice = access(&sign_in(&env, "alice").await);
    let (mut a, _) = identify(&env.ws_url(), &alice).await;
    let (request, _) = ask_to_join(&env, "erin", &mut a).await;
    let vouch = format!("/api/v1/access-requests/{request}/vouch");
    assert_eq!(
        post_auth(&env, &vouch, &alice, &json!({})).await.status,
        204
    );
    let res = join(&env, &erin, "erin_two").await;
    assert_message(&res, 409, "CONFLICT", "Email is already in use");
    assert_eq!(res.json()["error"]["detail"], "EMAIL_TAKEN");
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lobby_lock_refuses_join_requests_and_keeps_the_ticket() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let carol = ticket(&env, "carol@lobby.test", "ask_to_join").await;
    let (report, mut stopped) = env.stop().await;
    assert!(report.is_clean());
    stopped.config.auth.join_requests_enabled = false;
    let env = stopped.start().await;
    let info = get(env.addr, "/api/v1/lobby").await.json();
    assert_eq!(info["joinRequestsEnabled"], false);
    // A ticket minted while the Lobby took requests is refused once it is locked, and the refusal
    // rolls back, so it is not spent.
    for _ in 0..2 {
        let res = join(&env, &carol, "carol").await;
        assert_message(
            &res,
            403,
            "FORBIDDEN",
            "This Lobby is not accepting join requests",
        );
        assert_eq!(res.json()["error"]["detail"], "LOCKED");
    }
    // The code still goes out, and the lock shows only once it is checked.
    let code = request_code(&env, "dave@lobby.test").await;
    let res = verify(&env, &code.challenge, &code.code).await;
    assert_message(
        &res,
        403,
        "FORBIDDEN",
        "This Lobby is not accepting join requests",
    );
    assert_eq!(res.json()["error"]["detail"], "LOCKED");
    assert_eq!(verified(&env, &email_of("alice")).await["next"], "session");
    assert!(env.shutdown().await.is_clean());
}

/// Codes asked for the same address by others look alike in the inbox: the answer names the
/// request, and so does its mail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_request_is_named_in_its_answer_and_in_its_mail() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let carol = "carol@lobby.test";
    for n in 1..=2 {
        let res = post_json(
            env.addr,
            "/api/v1/auth/magic-code",
            &json!({"email": carol}),
        )
        .await;
        assert_eq!(res.status, 200, "{}", res.body);
        let label = res.json()["label"].as_str().unwrap().to_owned();
        assert!(
            label.len() == 3 && label.bytes().all(|b| b.is_ascii_uppercase()),
            "{label}"
        );
        env.mail.code_number(carol, n).await;
        let mail = env
            .mail
            .mails()
            .into_iter()
            .filter(|m| m.to == carol)
            .nth(n - 1);
        let body = mail.unwrap().data;
        assert!(
            body.contains(&format!("It is for request {label} and")),
            "{body}"
        );
    }
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codes_are_capped_per_email_and_client_and_each_challenge_takes_five_guesses() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        c.auth.magic_code_sends.per_email_per_10m = 3;
    })
    .await;
    let carol = "carol@lobby.test";
    let first = request_code(&env, carol).await;
    request_code(&env, carol).await;
    let third = request_code(&env, carol).await;
    // Three codes per 10 min to this email from this client: the fourth hears when to retry.
    let res = post_json(
        env.addr,
        "/api/v1/auth/magic-code",
        &json!({"email": carol}),
    )
    .await;
    assert_envelope(&res, 429, "RATE_LIMITED");
    let retry = res.json()["error"]["retryAfterMs"].as_u64().unwrap();
    assert!((1..=600_000).contains(&retry), "{retry}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.mail.count_to(carol), 3, "the fourth was not sent");
    // Another email from the same client has its own codes.
    request_code(&env, "dave@lobby.test").await;
    let wrong = wrong_code(&third.code);
    for _ in 0..5 {
        assert_message(
            &verify(&env, &third.challenge, &wrong).await,
            401,
            "AUTH_FAILED",
            "Invalid code",
        );
    }
    // Its five are spent: even the right code is refused without being compared.
    assert_message(
        &verify(&env, &third.challenge, &third.code).await,
        401,
        "AUTH_FAILED",
        "Too many attempts",
    );
    // Another challenge keeps its own attempts and its own code.
    let res = verify(&env, &first.challenge, &first.code).await;
    assert_eq!(res.status, 200, "{}", res.body);
    for (body, message) in [
        (json!({"code": "12345678"}), "missing field `challenge`"),
        (
            json!({"challenge": first.challenge.0, "code": "1234567"}),
            "invalid code length",
        ),
        (
            json!({"challenge": first.challenge.0, "code": "12a45678"}),
            "code must contain only digits",
        ),
    ] {
        let res = post_json(env.addr, "/api/v1/auth/magic-code/verify", &body).await;
        assert_envelope(&res, 400, "INVALID_REQUEST");
        assert!(
            res.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains(message),
            "{}",
            res.body
        );
    }
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_client_mails_codes_to_a_capped_number_of_emails_a_day() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        c.auth.magic_code_sends.per_client_per_24h = 3;
    })
    .await;
    for made_up in ["x1@lobby.test", "x2@lobby.test", "x3@lobby.test"] {
        request_code(&env, made_up).await;
    }
    // The fourth email hears when to retry, and a Member's hears the same.
    let mut messages = Vec::new();
    for email in [email_of("alice"), "x4@lobby.test".to_owned()] {
        let res = post_json(
            env.addr,
            "/api/v1/auth/magic-code",
            &json!({"email": email}),
        )
        .await;
        assert_envelope(&res, 429, "RATE_LIMITED");
        let retry = res.json()["error"]["retryAfterMs"].as_u64().unwrap();
        assert!((86_000_000..=86_400_000).contains(&retry), "{retry}");
        messages.push(res.json()["error"]["message"].clone());
    }
    assert_eq!(messages[0], messages[1]);
    assert!(env.shutdown().await.is_clean());
}

const STRANGER: Device = Device { ip: "203.0.113.9" };
const CAROL: Device = Device { ip: "198.51.100.7" };

fn behind_proxy(c: &mut Config) {
    c.http.trusted_proxy_cidrs = vec!["127.0.0.1/32".parse().unwrap()];
}

/// Many clients cannot bomb one inbox through `+tag` aliases, nor use up the mail provider's
/// quota between them. Past those caps a stranger's request is answered like any other with
/// nothing mailed, and a Member's code still goes out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codes_are_capped_per_mailbox_and_in_all_across_clients() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        behind_proxy(c);
        c.auth.magic_code_sends.per_mailbox_per_24h = 2;
        c.auth.magic_code_sends.per_lobby_per_24h = 4;
    })
    .await;
    let sources = (1..=5)
        .map(|n| format!("198.51.100.{n}"))
        .collect::<Vec<_>>();
    let from = |n: usize| Device { ip: &sources[n] };
    from(0).ask(&env, "carol+a@lobby.test").await;
    from(1).ask(&env, "Carol+b@lobby.test").await;
    from(2).ask(&env, "carol@lobby.test").await;
    from(2).ask(&env, "dave@lobby.test").await;
    from(3).ask(&env, "eve@lobby.test").await;
    from(4).ask(&env, "frank@lobby.test").await;

    let alice = email_of("alice");
    let asked = from(4).ask(&env, &alice).await;
    let code = env.mail.code_number(&alice, 1).await;
    let res = from(4).verify(&env, &asked, &code).await;
    assert_eq!(res.json()["next"], "session", "{}", res.body);

    assert_eq!(env.mail.count_to("carol@lobby.test"), 0);
    assert_eq!(env.mail.count_to("frank@lobby.test"), 0);
    assert!(env.shutdown().await.is_clean());
}

/// A stranger who asks for codes to Carol's email and guesses at them neither spends nor replaces
/// the code Carol asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stranger_cannot_burn_or_replace_a_members_code() {
    let env = TestServer::start_with_members(&["alice"], behind_proxy).await;
    let carol = "carol@lobby.test";
    let mine = CAROL.ask(&env, carol).await;
    let code = env.mail.code_number(carol, 1).await;
    let theirs = STRANGER.ask(&env, carol).await;
    let wrong = wrong_code(&code);
    for _ in 0..6 {
        let res = STRANGER.verify(&env, &theirs, &wrong).await;
        assert_envelope(&res, 401, "AUTH_FAILED");
    }
    let res = CAROL.verify(&env, &mine, &code).await;
    assert_eq!(res.status, 200, "{}", res.body);
    assert_eq!(res.json()["next"], "ask_to_join", "{}", res.body);
    assert!(env.shutdown().await.is_clean());
}

/// Under the shipped caps, strangers who fill a Member's mailbox and the Lobby's day of mail and
/// spend wrong guesses on those codes do not stop a code reaching her, nor her verify.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_strangers_requests_and_guesses_do_not_lock_the_member_out() {
    let caps = Config::default().auth.magic_code_sends;
    let env = TestServer::start_with_members(&["alice"], |c| {
        behind_proxy(c);
        c.auth.magic_code_sends = caps;
    })
    .await;
    for n in 0..caps.per_lobby_per_24h {
        let ip = format!("10.1.{}.{}", n / 200, n % 200 + 1);
        let stranger = Device { ip: &ip };
        let email = if n < caps.per_mailbox_per_24h {
            format!("alice+{n}@lobby.test")
        } else {
            format!("made-up{n}@lobby.test")
        };
        let theirs = stranger.ask(&env, &email).await;
        if n < 3 {
            let wrong = wrong_code(&env.mail.code_number(&email, 1).await);
            for _ in 0..5 {
                let res = stranger.verify(&env, &theirs, &wrong).await;
                assert_envelope(&res, 401, "AUTH_FAILED");
            }
        }
    }
    let alice = email_of("alice");
    let mine = CAROL.ask(&env, &alice).await;
    let code = env.mail.code_number(&alice, 1).await;
    let res = CAROL.verify(&env, &mine, &code).await;
    assert_eq!(res.json()["next"], "session", "{}", res.body);
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_out_closes_only_that_session_with_4005_and_everywhere_closes_all() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let here = sign_in(&env, "alice").await;
    let there = sign_in(&env, "alice").await;
    let (mut ws, _) = identify(&env.ws_url(), &access(&here)).await;
    let res = post_auth(&env, "/api/v1/auth/sign-out", &access(&here), &json!({})).await;
    assert_eq!(res.status, 204, "{}", res.body);
    assert_eq!(expect_close(&mut ws).await, 4005);
    assert_eq!(
        refresh(env.addr, &refresh_token(&here["tokens"]))
            .await
            .status,
        401
    );
    let fresh = refresh(env.addr, &refresh_token(&there["tokens"])).await;
    assert_eq!(fresh.status, 200, "{}", fresh.body);
    let fresh = fresh.json();

    let third = sign_in(&env, "alice").await;
    let (mut ws, _) = identify(&env.ws_url(), fresh["accessToken"].as_str().unwrap()).await;
    let res = post_auth(
        &env,
        "/api/v1/auth/sign-out",
        &access(&third),
        &json!({"everywhere": true}),
    )
    .await;
    assert_eq!(res.status, 204, "{}", res.body);
    assert_eq!(expect_close(&mut ws).await, 4005);
    assert_eq!(refresh(env.addr, &refresh_token(&fresh)).await.status, 401);
    assert_envelope(
        &post_auth(&env, "/api/v1/auth/sign-out", &access(&third), &json!({})).await,
        401,
        "AUTH_FAILED",
    );
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_routes_are_limited_per_ip_and_a_spoofed_forwarded_for_does_not_help() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        c.http.rate_limits = true;
        c.http.trusted_proxy_cidrs = vec!["127.0.0.1/32".parse().unwrap()];
    })
    .await;
    let guess = json!({"challenge": "c".repeat(43), "code": "00000000"}).to_string();
    let attempt = |xff: String| {
        let (addr, guess) = (env.addr, guess.clone());
        async move {
            http(
                addr,
                "POST",
                "/api/v1/auth/magic-code/verify",
                &[
                    ("Content-Type", "application/json"),
                    ("X-Forwarded-For", xff.as_str()),
                ],
                &guess,
            )
            .await
        }
    };
    // The proxy (127.0.0.1) appended the client 203.0.113.9; the left-most entry is the client's
    // own claim, a different one each time.
    for i in 0..5 {
        let res = attempt(format!("10.0.0.{i}, 203.0.113.9")).await;
        assert_eq!(res.status, 401, "{}", res.body);
    }
    let res = attempt("10.9.9.9, 203.0.113.9".into()).await;
    assert_envelope(&res, 429, "RATE_LIMITED");
    let retry: u64 = res.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    assert!(res.json()["error"]["retryAfterMs"].as_u64().unwrap() > 0);
    assert_eq!(attempt("203.0.113.10".into()).await.status, 401);
    let res = post_json(
        env.addr,
        "/api/v1/auth/magic-code",
        &json!({"email": "x@lobby.test"}),
    )
    .await;
    assert_eq!(res.status, 200);
    assert!(env.shutdown().await.is_clean());

    // Without a trusted proxy the header is ignored: every attempt counts for the socket's IP.
    let env = TestServer::start_with_members(&["alice"], |c| c.http.rate_limits = true).await;
    for i in 0..5 {
        let res = http(
            env.addr,
            "POST",
            "/api/v1/auth/magic-code/verify",
            &[
                ("Content-Type", "application/json"),
                ("X-Forwarded-For", &format!("198.51.100.{i}")),
            ],
            &guess,
        )
        .await;
        assert_eq!(res.status, 401, "{}", res.body);
    }
    let res = post_json(
        env.addr,
        "/api/v1/auth/magic-code/verify",
        &json!({"challenge": "c".repeat(43), "code": "00000000"}),
    )
    .await;
    assert_envelope(&res, 429, "RATE_LIMITED");
    assert!(env.shutdown().await.is_clean());
}

/// Waiting for a Vouch checks with its wait token, spending no code: pending until a Member
/// Vouches, then the first check signs that device in and spends the token; a Decline says so; a
/// wait token from a verify works the same; anything else is a 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_for_a_vouch_turns_into_a_session_without_a_new_code() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let (mut a, _) = identify(&env.ws_url(), &access(&sign_in(&env, "alice").await)).await;
    let (carol, wait) = ask_to_join(&env, "carol", &mut a).await;
    let codes_before = env.mail.count_to(&email_of("carol"));
    for _ in 0..3 {
        let res = join_status(&env, &wait).await;
        assert_eq!(res.status, 200, "{}", res.body);
        assert_eq!(res.json(), json!({"next": "pending_approval"}));
    }
    let other_device = wait_token(&verified(&env, &email_of("carol")).await);
    let alice = access(&sign_in(&env, "alice").await);
    let vouch = format!("/api/v1/access-requests/{carol}/vouch");
    assert_eq!(
        post_auth(&env, &vouch, &alice, &json!({})).await.status,
        204
    );
    let res = join_status(&env, &wait).await;
    assert_eq!(res.status, 200, "{}", res.body);
    let signed_in = res.json();
    assert_eq!(signed_in["next"], "session", "{signed_in}");
    assert_eq!(signed_in["session"]["user"]["username"], "carol");
    let (_ws, ready) = identify(&env.ws_url(), &access(&signed_in["session"])).await;
    assert_eq!(ready["me"], signed_in["session"]["user"]["id"]);
    assert_eq!(ready["email"], email_of("carol"));
    assert_message(
        &join_status(&env, &wait).await,
        401,
        "AUTH_FAILED",
        "Invalid or expired wait token",
    );
    assert_eq!(
        join_status(&env, &other_device).await.json()["next"],
        "session"
    );
    assert_eq!(
        env.mail.count_to(&email_of("carol")),
        codes_before + 1,
        "only the other device's code was mailed"
    );

    let (dave, dave_wait) = ask_to_join(&env, "dave", &mut a).await;
    let decline = format!("/api/v1/access-requests/{dave}/decline");
    assert_eq!(
        post_auth(&env, &decline, &alice, &json!({})).await.status,
        204
    );
    assert_eq!(
        join_status(&env, &dave_wait).await.json(),
        json!({"next": "declined"})
    );
    assert_envelope(
        &join_status(&env, "not-a-wait-token").await,
        401,
        "AUTH_FAILED",
    );
    assert!(env.shutdown().await.is_clean());
}
