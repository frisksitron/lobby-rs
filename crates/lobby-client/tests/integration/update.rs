//! The updater against a release feed on loopback: a newer release downloads with progress, is
//! checked against its manifest and swaps in; a corrupt one is thrown away; the current one is
//! never downloaded.

use std::{fs, time::Duration};

use lobby_client::{
    MemoryTokenStore,
    testing::{FakeReleases, test_client},
    update::{Manifest, UpdateError, UpdateStatus, UpdateTiming, Version, sha256_hex, swap_in},
};
use tokio::sync::watch;

use crate::common::WAIT;

const CURRENT: Version = Version {
    major: 0,
    minor: 9,
    patch: 4,
};
const NEWER: Version = Version {
    major: 0,
    minor: 10,
    patch: 0,
};

fn timing() -> UpdateTiming {
    UpdateTiming {
        first_check: Duration::ZERO,
        check_every: Duration::from_secs(24 * 60 * 60),
    }
}

/// An app big enough to arrive in many chunks.
fn app() -> Vec<u8> {
    (0..600_000u32).map(|i| (i % 251) as u8).collect()
}

async fn until(
    status: &mut watch::Receiver<UpdateStatus>,
    done: impl Fn(&UpdateStatus) -> bool,
) -> UpdateStatus {
    tokio::time::timeout(WAIT, async {
        loop {
            let now = status.borrow_and_update().clone();
            if done(&now) {
                return now;
            }
            status.changed().await.expect("the updater runs");
        }
    })
    .await
    .expect("the updater settles")
}

#[tokio::test]
async fn a_newer_release_downloads_with_progress_and_swaps_in() {
    let asset = app();
    let manifest = Manifest {
        version: NEWER,
        size: asset.len() as u64,
        sha256: sha256_hex(&asset),
    };
    let releases = FakeReleases::holding_halfway(manifest, asset.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let client = test_client(&MemoryTokenStore::new());
    let updater = client.updater(releases.feed(), CURRENT, dir.path().to_owned(), timing());
    let mut status = updater.status();

    until(&mut status, |s| {
        matches!(s, UpdateStatus::Downloading { received, total, .. }
            if *received > 0 && received < total)
    })
    .await;
    releases.let_rest_through();
    let ready = until(&mut status, |s| matches!(s, UpdateStatus::Ready { .. })).await;
    let UpdateStatus::Ready { version, file } = ready else {
        unreachable!()
    };
    assert_eq!(version, NEWER);

    let exe = dir.path().join("Lobby.exe");
    fs::write(&exe, b"the running app").unwrap();
    swap_in(&exe, &file).unwrap();
    assert_eq!(fs::read(&exe).unwrap(), asset);
    assert_eq!(releases.downloads(), 1);
}

#[tokio::test]
async fn a_download_that_does_not_match_its_manifest_is_thrown_away() {
    let asset = app();
    let manifest = Manifest {
        version: NEWER,
        size: asset.len() as u64,
        sha256: sha256_hex(b"some other app"),
    };
    let releases = FakeReleases::start(manifest, asset).await;
    let dir = tempfile::tempdir().unwrap();
    let client = test_client(&MemoryTokenStore::new());
    let updater = client.updater(releases.feed(), CURRENT, dir.path().to_owned(), timing());

    let failed = until(&mut updater.status(), |s| {
        matches!(s, UpdateStatus::Failed { .. })
    })
    .await;
    assert_eq!(
        failed,
        UpdateStatus::Failed {
            version: NEWER,
            error: UpdateError::Corrupt
        }
    );
    let left: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
    assert!(left.is_empty(), "nothing is kept: {left:?}");
}

#[tokio::test]
async fn the_running_release_is_never_downloaded() {
    let asset = app();
    let manifest = Manifest {
        version: CURRENT,
        size: asset.len() as u64,
        sha256: sha256_hex(&asset),
    };
    let releases = FakeReleases::start(manifest, asset).await;
    let dir = tempfile::tempdir().unwrap();
    let client = test_client(&MemoryTokenStore::new());
    let updater = client.updater(releases.feed(), CURRENT, dir.path().to_owned(), timing());

    // The second manifest is served only once the first check is over.
    tokio::time::timeout(WAIT, async {
        while releases.manifests_served() < 2 {
            updater.check_now();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("two checks");
    assert_eq!(releases.downloads(), 0);
    assert_eq!(*updater.status().borrow(), UpdateStatus::Current);
}
