//! End to end: the real server with its `SmtpSink`, real `Client`s with their Entries and
//! Sessions run by the same drivers as the desktop's. The ways in after a Magic Code (a Member
//! signs in, the bootstrap email sets the Lobby up, anyone else Asks to Join), a Vouch from a
//! Member's session (and Waiting for a Vouch turning into a session without a new code), Sign
//! Out, a refresh response lost between the server's commit and the client's keychain write, and
//! the owner removing a Member and handing ownership on, and the owner leaving in one step.

use std::time::Duration;

use lobby_client::{
    AccountOp, Closed, EntryIntent, EntryStep, Intent, IssueKind, JOIN_CHECK_EVERY,
    MemoryTokenStore, NamePath, OwnerAction, Phase, Said, SignedOutWhy, TokenStore as _, UiEffect,
    testing::{ChaosProxy, ManualClock, test_client},
};
use lobby_protocol::Presence;
use lobby_server::testing::{TestServer, email_of};

use crate::common::{Entering, Member, origin};

const ADMIN: &str = "admin@lobby.test";

/// From the Lobby's email step to the code step for `email`, with the number of its code.
async fn to_code(
    env: &TestServer,
    client: &lobby_client::Client,
    email: &str,
) -> (Entering, usize) {
    let mut entry = Entering::at(client, &origin(env)).await;
    let n = env.mail.count_to(email) + 1;
    entry.send_code(email).await;
    (entry, n)
}

/// A code for an email no Member uses, then a name: Signed in for the bootstrap email of a new
/// Lobby, else Waiting for a Vouch.
async fn pick_name(
    env: &TestServer,
    client: &lobby_client::Client,
    email: &str,
    username: &str,
    path: NamePath,
) -> Entering {
    let (mut entry, n) = to_code(env, client, email).await;
    let view = entry.enter_code(env, email, n).await;
    assert!(
        matches!(&view.step, EntryStep::Name { path: p, .. } if *p == path),
        "the Lobby asks a new email for a name: {view:?}"
    );
    let n = entry.act(EntryIntent::PickName(username.to_owned()));
    entry.answered(n).await;
    entry
}

/// Check now, pressed, on Waiting for a Vouch: the step it leads to, or `None` for "not yet".
async fn check(entry: &mut Entering) -> Option<EntryStep> {
    let epoch = entry.view().epoch;
    let n = entry.act(EntryIntent::CheckNow);
    let view = entry.answered(n).await;
    (view.epoch != epoch).then(|| view.step.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_ask_to_join_and_a_vouch_bring_a_new_member_in_live() {
    let env = TestServer::start_with(|c| c.auth.bootstrap_email = ADMIN.into()).await;
    let origin = origin(&env);
    let carol = email_of("carol");
    let carol_store = MemoryTokenStore::new();
    let clock = ManualClock::new();
    let carol_client = test_client(&carol_store).with_clock(clock.clone());

    let (mut early, n) = to_code(&env, &carol_client, &carol).await;
    let view = early.enter_code(&env, &carol, n).await;
    assert!(
        matches!(
            view.step,
            EntryStep::Closed {
                why: Closed::NotOpenYet,
                ..
            }
        ),
        "an early email waits for the owner: {view:?}"
    );
    drop(early);

    let admin_store = MemoryTokenStore::new();
    let admin_client = test_client(&admin_store);
    let mut joined = pick_name(&env, &admin_client, ADMIN, "admin", NamePath::CreateLobby).await;
    assert_eq!(
        joined.view().step,
        EntryStep::SignedIn,
        "the first Member is in"
    );
    assert!(
        admin_store.peek(&origin).is_some(),
        "tokens stored before use"
    );
    let mut admin = Member::start(&admin_client, &origin);
    admin.wait_connected().await;

    let mut asked = pick_name(&env, &carol_client, &carol, "carol", NamePath::AskToJoin).await;
    assert!(
        matches!(asked.view().step, EntryStep::AwaitingVouch { .. }),
        "carol waits for a Vouch"
    );
    assert!(carol_store.peek(&origin).is_none());
    admin
        .wait_until("carol's request", |s| {
            s.access_requests
                .iter()
                .any(|r| r.username.as_str() == "carol")
        })
        .await;
    let (mut again, n) = to_code(&env, &carol_client, &carol).await;
    let view = again.enter_code(&env, &carol, n).await;
    assert!(
        matches!(view.step, EntryStep::AwaitingVouch { name: None, .. }),
        "a code from a returning asker shows where the request stands: {view:?}"
    );
    drop(again);
    assert_eq!(check(&mut asked).await, None, "not yet");
    assert_eq!(
        asked.view().said,
        Some(Said::Hint(lobby_client::copy::NOT_YET.into()))
    );

    admin.vouch("carol").await;
    assert!(!admin.state().deciding.is_empty(), "the buttons wait");
    admin
        .wait_until("carol vouched in", |s| {
            s.access_requests.is_empty()
                && s.deciding.is_empty()
                && s.members.values().any(|m| m.username.as_str() == "carol")
        })
        .await;

    let mailed = env.mail.count_to(&carol);
    // Nobody presses Check now: the next check by itself finds the Vouch.
    clock.advance(JOIN_CHECK_EVERY);
    asked
        .wait("signed in by the next check", |v| {
            v.step == EntryStep::SignedIn
        })
        .await;
    assert!(carol_store.peek(&origin).is_some(), "tokens stored");
    assert_eq!(env.mail.count_to(&carol), mailed, "no code was needed");
    let mut carol_session = Member::start(&carol_client, &origin);
    carol_session.wait_connected().await;
    admin
        .wait_until("carol online", |s| {
            s.members
                .values()
                .any(|m| m.username.as_str() == "carol" && m.presence == Presence::Online)
        })
        .await;

    let laptop_store = MemoryTokenStore::new();
    let laptop = test_client(&laptop_store);
    let (mut on_laptop, n) = to_code(&env, &laptop, &carol).await;
    assert_eq!(
        on_laptop.enter_code(&env, &carol, n).await.step,
        EntryStep::SignedIn,
        "a Member signs in on another device with a Magic Code"
    );
    assert!(laptop_store.peek(&origin).is_some(), "tokens stored");

    let dave_client = test_client(&MemoryTokenStore::new());
    let dave_email = email_of("dave");
    let mut dave = pick_name(&env, &dave_client, &dave_email, "dave", NamePath::AskToJoin).await;
    assert!(matches!(dave.view().step, EntryStep::AwaitingVouch { .. }));
    admin
        .wait_until("dave's request", |s| !s.access_requests.is_empty())
        .await;
    admin.decline("dave").await;
    admin
        .wait_until("dave declined", |s| {
            s.access_requests.is_empty() && s.deciding.is_empty()
        })
        .await;
    assert!(
        !admin
            .state()
            .members
            .values()
            .any(|m| m.username.as_str() == "dave")
    );
    assert!(
        !admin
            .take_effects()
            .iter()
            .any(|e| matches!(e, UiEffect::Issue(_))),
        "a successful decision raises nothing"
    );
    assert_eq!(
        check(&mut dave).await,
        Some(EntryStep::Closed {
            why: Closed::Declined,
            email: lobby_protocol::Email::parse(&dave_email).unwrap(),
            name: Some("dave".parse().unwrap()),
        })
    );
    carol_session.shutdown().await;
    admin.shutdown().await;
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_out_ends_only_this_device() {
    let env = TestServer::start_with_members(&["alice", "bob"], |_| {}).await;
    let origin = origin(&env);
    let bob_client = test_client(&MemoryTokenStore::new());
    let mut bob = Member::sign_in(&bob_client, &origin, "bob", &env.mail)
        .await
        .unwrap();

    let alice_store = MemoryTokenStore::new();
    let alice_client = test_client(&alice_store);
    let alice = Member::sign_in(&alice_client, &origin, "alice", &env.mail)
        .await
        .unwrap();
    bob.wait_until("alice online", |s| {
        s.members
            .values()
            .any(|m| m.username.as_str() == "alice" && m.presence == Presence::Online)
    })
    .await;
    let old = alice_store.peek(&origin).unwrap();
    alice.sign_out().await;
    assert!(
        alice_store.peek(&origin).is_none(),
        "this device's tokens are cleared"
    );
    bob.wait_until("alice offline", |s| {
        s.members
            .values()
            .any(|m| m.username.as_str() == "alice" && m.presence == Presence::Offline)
    })
    .await;
    alice_store.save(&origin, &old).unwrap();
    let mut stale = Member::start(&alice_client, &origin);
    stale.wait_phase(Phase::NeedsAuth).await;
    stale.shutdown().await;
    assert_eq!(bob.state().phase, Phase::Connected, "bob stays in");
    bob.shutdown().await;
    assert!(env.shutdown().await.is_clean());
}

/// The client dies between the server's rotation commit and its keychain write (the response
/// never arrives), however often it retries meanwhile; started again, it presents the old refresh
/// token and the server reissues: no Sign In.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_refresh_response_never_signs_the_member_out() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        // Every use refreshes first (it expires within the 60 s lead).
        c.auth.access_ttl = Duration::from_secs(30);
    })
    .await;
    let store = MemoryTokenStore::new();
    let origin = origin(&env);
    let alice = email_of("alice");
    let (mut entry, n) = to_code(&env, &test_client(&store), &alice).await;
    assert_eq!(
        entry.enter_code(&env, &alice, n).await.step,
        EntryStep::SignedIn
    );
    let before = store.peek(&origin).unwrap();

    let proxy = ChaosProxy::start(env.addr).await;
    proxy.drop_responses_to(Some("POST /api/v1/auth/refresh"));
    let via_proxy = proxy.origin();
    store.save(&via_proxy, &before).unwrap();
    let doomed = test_client(&store);
    let mut session = Member::start(&doomed, &via_proxy);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while proxy.dropped() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the refresh went out and was lost"
        );
        session.run_for(Duration::from_millis(50)).await;
    }
    assert_eq!(
        store.peek(&via_proxy).as_ref(),
        Some(&before),
        "nothing new was written"
    );
    assert_ne!(session.state().phase, Phase::NeedsAuth);
    drop(session);
    drop(doomed);

    store.save(&origin, &before).unwrap();
    let mut back = Member::start(&test_client(&store), &origin);
    back.wait_connected().await;
    assert!(
        !back
            .take_effects()
            .iter()
            .any(|e| matches!(e, UiEffect::NeedsAuth(_)))
    );
    let after = store.peek(&origin).unwrap();
    assert_ne!(
        after.refresh_token, before.refresh_token,
        "a fresh pair, persisted"
    );
    back.shutdown().await;
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_removes_a_member_who_returns_and_hands_ownership_on() {
    let env = TestServer::start_with_members(&["alice", "bob", "carol"], |_| {}).await;
    let origin = origin(&env);
    let alice_client = test_client(&MemoryTokenStore::new());
    let mut alice = Member::sign_in(&alice_client, &origin, "alice", &env.mail)
        .await
        .unwrap();
    let bob_store = MemoryTokenStore::new();
    let bob_client = test_client(&bob_store);
    let mut bob = Member::sign_in(&bob_client, &origin, "bob", &env.mail)
        .await
        .unwrap();
    let carol_client = test_client(&MemoryTokenStore::new());
    let mut carol = Member::sign_in(&carol_client, &origin, "carol", &env.mail)
        .await
        .unwrap();
    assert!(alice.state().is_owner() && !bob.state().is_owner());
    let (alice_id, bob_id, carol_id) = (
        alice.id_of("alice"),
        alice.id_of("bob"),
        alice.id_of("carol"),
    );

    assert_eq!(
        bob.owner_action(&carol_id, OwnerAction::Remove).await,
        Err("carol: only the Lobby's owner can do that.".into())
    );
    assert_eq!(
        alice.owner_action(&alice_id, OwnerAction::Remove).await,
        Err("alice: the Lobby's owner can't be removed.".into())
    );

    assert_eq!(
        alice.owner_action(&bob_id, OwnerAction::Remove).await,
        Ok(())
    );
    bob.wait_phase(Phase::NeedsAuth).await;
    assert!(
        bob.take_effects()
            .contains(&UiEffect::NeedsAuth(SignedOutWhy::Removed)),
        "4006: removed, not expired"
    );
    assert!(bob_store.peek(&origin).is_none(), "his tokens are gone");
    bob.shutdown().await;
    for who in [&mut alice, &mut carol] {
        who.wait_until("bob gone", |s| !s.members.contains_key(&bob_id))
            .await;
    }
    let mut asked = pick_name(
        &env,
        &bob_client,
        &email_of("bob"),
        "bob",
        NamePath::AskToJoin,
    )
    .await;
    assert!(
        matches!(asked.view().step, EntryStep::AwaitingVouch { .. }),
        "bob waits for a Vouch"
    );
    alice
        .wait_until("bob's request", |s| !s.access_requests.is_empty())
        .await;
    alice.vouch("bob").await;
    carol
        .wait_until("bob back", |s| s.members.contains_key(&bob_id))
        .await;
    assert_eq!(check(&mut asked).await, Some(EntryStep::SignedIn));
    let mut bob = Member::start(&bob_client, &origin);
    bob.wait_connected().await;
    assert_eq!(bob.id_of("bob"), bob_id, "himself again");

    assert_eq!(
        alice.owner_action(&carol_id, OwnerAction::MakeOwner).await,
        Ok(())
    );
    for who in [&mut alice, &mut bob, &mut carol] {
        who.wait_until("carol owns the Lobby", |s| s.owner() == Some(&carol_id))
            .await;
    }
    assert!(carol.state().is_owner() && !alice.state().is_owner());
    assert_eq!(
        alice.owner_action(&bob_id, OwnerAction::Remove).await,
        Err("bob: only the Lobby's owner can do that.".into())
    );
    for who in [alice, bob, carol] {
        who.shutdown().await;
    }
    assert!(env.shutdown().await.is_clean());
}

/// The owner's Leave is one Intent: a hand-over the Lobby refuses leaves the owner where they
/// were; an accepted one makes the heir the owner and then lets the old owner go.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owner_hands_the_lobby_on_and_leaves_in_one_step() {
    let env = TestServer::start_with_members(&["alice", "bob", "carol"], |_| {}).await;
    let origin = origin(&env);
    let alice_client = test_client(&MemoryTokenStore::new());
    let mut alice = Member::sign_in(&alice_client, &origin, "alice", &env.mail)
        .await
        .unwrap();
    let carol_client = test_client(&MemoryTokenStore::new());
    let mut carol = Member::sign_in(&carol_client, &origin, "carol", &env.mail)
        .await
        .unwrap();
    let (alice_id, bob_id, carol_id) = (
        alice.id_of("alice"),
        alice.id_of("bob"),
        alice.id_of("carol"),
    );
    assert_eq!(
        alice.owner_action(&bob_id, OwnerAction::Remove).await,
        Ok(())
    );
    alice
        .wait_until("bob gone", |s| !s.members.contains_key(&bob_id))
        .await;

    let _ = alice.act(Intent::HandOverAndLeave(bob_id)).await;
    let refused = alice.wait_issue(IssueKind::TransferFailed).await;
    assert_eq!(refused, "They are no longer a Member.");
    alice
        .wait_until("the Leave ended", |s| !s.account.running(AccountOp::Leave))
        .await;
    assert!(alice.state().is_owner(), "still the owner");
    assert_eq!(alice.state().phase, Phase::Connected, "still in");

    let _ = alice.act(Intent::HandOverAndLeave(carol_id.clone())).await;
    alice
        .wait_until("the Leave ended", |s| !s.account.running(AccountOp::Leave))
        .await;
    assert!(
        alice.take_effects().contains(&UiEffect::Left),
        "the Lobby let her go"
    );
    carol
        .wait_until("carol owns it and alice is gone", |s| {
            s.owner() == Some(&carol_id) && !s.members.contains_key(&alice_id)
        })
        .await;
    for who in [alice, carol] {
        who.shutdown().await;
    }
    assert!(env.shutdown().await.is_clean());
}
