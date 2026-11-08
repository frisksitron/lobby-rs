//! Entry's flows against the real server: Signed in only once the tokens are stored, Back while a
//! sign in is still on its way, Sign Out while a token save still runs, the resend rule, Ask to
//! Join with a name that is taken, and the rail of done steps a press goes back to.

use std::time::Duration;

use lobby_client::{
    Chip, ChipAt, Client, EntryIntent, EntryStep, Fill, MemoryTokenStore, NamePath, RESEND_AFTER,
    Resend, Said, Stage, copy,
    testing::{ManualClock, test_client},
};
use lobby_server::testing::{TestServer, email_of};

use crate::common::{Entering, Member, origin};

/// Long enough for any keychain call this machine makes, short enough to wait out in a test.
const STORE_TIMEOUT: Duration = Duration::from_millis(200);

async fn until(what: &str, cond: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}: not within 5 s"));
}

/// At the code step for `name`'s email, with the code the Lobby mailed for it.
async fn at_code(env: &TestServer, client: &Client, name: &str) -> (Entering, EntryIntent) {
    let email = email_of(name);
    let mut entry = Entering::at(client, &origin(env)).await;
    let n = env.mail.count_to(&email) + 1;
    entry.send_code(&email).await;
    let code = env.mail.code_number(&email, n).await;
    (entry, EntryIntent::SubmitCode(code))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_in_shows_only_once_the_tokens_are_stored_and_a_failed_store_keeps_the_step() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let origin = origin(&env);

    let store = MemoryTokenStore::new();
    let (mut entry, code) = at_code(&env, &test_client(&store), "alice").await;
    store.hold_saves(true);
    let n = entry.act(code);
    until("the save is under way", || store.saves_waiting() == 1).await;
    let view = entry.view();
    assert!(matches!(view.step, EntryStep::Code { .. }) && view.busy);
    store.hold_saves(false);
    assert_eq!(entry.answered(n).await.step, EntryStep::SignedIn);
    assert!(store.peek(&origin).is_some(), "stored before Signed in");

    let failing = MemoryTokenStore::new();
    let (mut entry, code) = at_code(&env, &test_client(&failing), "alice").await;
    failing.set_failing(true);
    let n = entry.act(code);
    let view = entry.answered(n).await;
    assert!(
        matches!(view.step, EntryStep::Code { .. }),
        "the step stays"
    );
    assert_eq!(
        view.said,
        Some(Said::Error(copy::KEYCHAIN_UNAVAILABLE.into()))
    );

    // A keychain that does not answer in time fails the store too, and the save that lands after
    // it leaves nothing behind.
    let slow = MemoryTokenStore::new();
    let client = test_client(&slow).with_store_timeout(STORE_TIMEOUT);
    let (mut entry, code) = at_code(&env, &client, "alice").await;
    slow.hold_saves(true);
    let n = entry.act(code);
    let view = entry.answered(n).await;
    assert!(matches!(view.step, EntryStep::Code { .. }));
    assert_eq!(
        view.said,
        Some(Said::Error(copy::KEYCHAIN_UNAVAILABLE.into()))
    );
    slow.hold_saves(false);
    until("the late save is undone", || {
        slow.saves_waiting() == 0 && slow.peek(&origin).is_none()
    })
    .await;
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn going_back_during_a_slow_sign_in_stays_where_you_went_and_stores_nothing() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let origin = origin(&env);
    let store = MemoryTokenStore::new();
    let client = test_client(&store);

    let (mut entry, code) = at_code(&env, &client, "alice").await;
    store.hold_saves(true);
    entry.act(code);
    until("the save is under way", || store.saves_waiting() == 1).await;
    let n = entry.act(EntryIntent::Back);
    assert_eq!(entry.answered(n).await.step, EntryStep::Email);
    store.hold_saves(false);
    until("the save is undone after Back", || {
        store.saves_waiting() == 0 && store.peek(&origin).is_none()
    })
    .await;
    assert_eq!(entry.view().step, EntryStep::Email, "and it stays there");

    // Leaving Sign In altogether drops its Entry: the same.
    let (mut entry, code) = at_code(&env, &client, "alice").await;
    store.hold_saves(true);
    entry.act(code);
    until("the save is under way", || store.saves_waiting() == 1).await;
    drop(entry);
    store.hold_saves(false);
    until("the save is undone after Entry was dropped", || {
        store.saves_waiting() == 0 && store.peek(&origin).is_none()
    })
    .await;
    assert!(env.shutdown().await.is_clean());
}

/// Sign Out reads a fresh token first, and with a 30 s token every read refreshes: the refresh's
/// save is held past the keychain's timeout, the clear runs, then the save lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signing_out_while_a_token_save_still_runs_ends_signed_out() {
    let env = TestServer::start_with_members(&["alice"], |c| {
        c.auth.access_ttl = Duration::from_secs(30);
    })
    .await;
    let origin = origin(&env);
    let store = MemoryTokenStore::new();
    let client = test_client(&store).with_store_timeout(STORE_TIMEOUT);
    let alice = Member::sign_in(&client, &origin, "alice", &env.mail)
        .await
        .unwrap();
    store.hold_saves(true);
    alice.sign_out().await;
    assert!(store.saves_waiting() > 0, "a token save still runs");
    store.hold_saves(false);
    until("signed out", || {
        store.saves_waiting() == 0 && store.peek(&origin).is_none()
    })
    .await;
    assert!(env.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_code_waits_then_is_ready_then_is_capped() {
    const CAP: u32 = 3;
    let env = TestServer::start_with_members(&["alice"], |c| {
        c.auth.magic_code_sends.per_email_per_10m = CAP;
    })
    .await;
    let clock = ManualClock::new();
    let client = test_client(&MemoryTokenStore::new()).with_clock(clock.clone());
    let email = email_of("carol");
    let mut entry = Entering::at(&client, &origin(&env)).await;
    entry.send_code(&email).await;
    for sent in 1..CAP as usize {
        assert_eq!(entry.view().resend, Resend::Wait, "after {sent} codes");
        clock.advance(RESEND_AFTER);
        entry
            .wait("a new code is ready", |v| v.resend == Resend::Ready)
            .await;
        let n = entry.act(EntryIntent::Resend);
        let view = entry.answered(n).await;
        let said = if sent + 1 == CAP as usize {
            Said::Info(copy::codes_capped(CAP, view.code_label.as_ref()))
        } else {
            Said::Notice(copy::CODE_RESENT.into())
        };
        assert_eq!(view.said, Some(said));
    }
    clock.advance(RESEND_AFTER);
    let n = entry.act(EntryIntent::Resend);
    let view = entry.answered(n).await;
    assert_eq!(view.resend, Resend::Capped, "no more codes");
    until("the codes mailed", || {
        env.mail.count_to(&email) == CAP as usize
    })
    .await;
    // The app names the request of the newest code, and that code's mail does too.
    let label = view
        .code_label
        .clone()
        .expect("the code step names its request");
    let newest = env
        .mail
        .mails()
        .into_iter()
        .rfind(|m| m.to == email)
        .unwrap();
    assert!(
        newest.data.contains(&format!("for request {label} and")),
        "{}",
        newest.data
    );
    assert!(env.shutdown().await.is_clean());
}

/// Each Lobby caps its own codes: three asked of Lobby A do not stop Lobby B mailing one, or
/// make B's code step wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_codes_asked_of_one_lobby_do_not_count_at_the_next() {
    const CAP: u32 = 3;
    let cap = |c: &mut lobby_server::Config| c.auth.magic_code_sends.per_email_per_10m = CAP;
    let a = TestServer::start_with_members(&["alice"], cap).await;
    let b = TestServer::start_with_members(&["alice"], cap).await;
    let clock = ManualClock::new();
    let client = test_client(&MemoryTokenStore::new()).with_clock(clock.clone());
    let email = email_of("carol");
    let mut entry = Entering::at(&client, &origin(&a)).await;
    entry.send_code(&email).await;
    for _ in 1..CAP {
        clock.advance(RESEND_AFTER);
        let n = entry.act(EntryIntent::Resend);
        entry.answered(n).await;
    }
    assert_eq!(entry.view().resend, Resend::Capped, "Lobby A's cap");

    for _ in 0..2 {
        let n = entry.act(EntryIntent::Back);
        entry.answered(n).await;
    }
    assert_eq!(entry.view().step, EntryStep::Address);
    entry.act(EntryIntent::Connect(origin(&b).to_string()));
    entry
        .wait("Lobby B answered", |v| v.step == EntryStep::Email)
        .await;
    entry.send_code(&email).await;
    assert_eq!(entry.view().resend, Resend::Wait, "one code from Lobby B");
    until("Lobby B mailed its code", || b.mail.count_to(&email) == 1).await;
    until("Lobby A mailed its codes", || {
        a.mail.count_to(&email) == CAP as usize
    })
    .await;
    assert!(a.shutdown().await.is_clean());
    assert!(b.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_to_join_with_a_name_that_is_taken_is_refused_and_another_needs_no_new_code() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let email = email_of("carol");
    let client = test_client(&MemoryTokenStore::new());
    let (mut entry, code) = at_code(&env, &client, "carol").await;
    let n = entry.act(code);
    let view = entry.answered(n).await;
    assert!(
        matches!(
            view.step,
            EntryStep::Name {
                path: NamePath::AskToJoin,
                ..
            }
        ),
        "{view:?}"
    );
    assert_eq!(view.fill, Some(Fill::Username("carol".into())));
    let n = entry.act(EntryIntent::PickName("alice".into()));
    let view = entry.answered(n).await;
    assert!(matches!(view.step, EntryStep::Name { .. }), "{view:?}");
    assert_eq!(view.said, Some(Said::Error(copy::USERNAME_TAKEN.into())));
    let n = entry.act(EntryIntent::PickName("carol".into()));
    let view = entry.answered(n).await;
    let EntryStep::AwaitingVouch { name, .. } = &view.step else {
        panic!("carol waits for a Vouch: {view:?}");
    };
    assert_eq!(name.as_ref().map(|n| n.as_str()), Some("carol"));
    assert_eq!(env.mail.count_to(&email), 1, "no new code");
    assert!(env.shutdown().await.is_clean());
}

/// The rail carries each done step's value. A press on the Lobby or the email goes back there
/// with it filled in; the code, once spent, is not a step to go back to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_rail_shows_each_done_step_and_a_press_on_one_goes_back_to_it() {
    let env = TestServer::start_with_members(&["alice"], |_| {}).await;
    let origin = origin(&env);
    let host = origin.host().to_owned();
    let client = test_client(&MemoryTokenStore::new());
    let done = |stage, value: Option<&str>, back| Chip {
        stage,
        at: ChipAt::Done {
            value: value.map(str::to_owned),
            back,
        },
    };
    let chip = |stage, at| Chip { stage, at };

    let (mut entry, _) = at_code(&env, &client, "dave").await;
    let dave = email_of("dave");
    assert_eq!(
        entry.view().rail,
        [
            done(Stage::Lobby, Some(&host), true),
            done(Stage::Email, Some(&dave), true),
            chip(Stage::Code, ChipAt::Current),
        ]
    );
    let n = entry.act(EntryIntent::GoTo(Stage::Email));
    let view = entry.answered(n).await;
    assert_eq!(view.step, EntryStep::Email);
    assert_eq!(
        view.fill,
        Some(Fill::Email(lobby_protocol::Email::parse(&dave).unwrap()))
    );
    assert_eq!(
        view.rail[1..],
        [
            chip(Stage::Email, ChipAt::Current),
            chip(Stage::Code, ChipAt::Ahead)
        ]
    );

    let carol = email_of("carol");
    let n = env.mail.count_to(&carol) + 1;
    entry.send_code(&carol).await;
    let view = entry.enter_code(&env, &carol, n).await;
    assert!(matches!(view.step, EntryStep::Name { .. }), "{view:?}");
    assert_eq!(
        view.rail,
        [
            done(Stage::Lobby, Some(&host), true),
            done(Stage::Email, Some(&carol), true),
            done(Stage::Code, None, false),
            chip(Stage::Name, ChipAt::Current),
        ]
    );
    let n = entry.act(EntryIntent::GoTo(Stage::Code));
    assert!(
        matches!(entry.answered(n).await.step, EntryStep::Name { .. }),
        "a spent code is no step to go back to"
    );
    let n = entry.act(EntryIntent::GoTo(Stage::Lobby));
    let view = entry.answered(n).await;
    assert_eq!(view.step, EntryStep::Address);
    assert_eq!(view.origin.as_ref(), Some(&origin), "its address stays");
    assert!(env.shutdown().await.is_clean());
}
