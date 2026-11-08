//! A release feed on loopback, laid out like GitHub Releases, that counts what it served.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use tokio::{io::AsyncWriteExt as _, net::TcpListener, sync::Notify};
use tokio_util::task::AbortOnDropHandle;

use super::http::{read_request, respond};
use crate::update::{ASSET, MANIFEST, Manifest, ReleaseFeed};

pub struct FakeReleases {
    base: String,
    shared: Arc<Shared>,
    _serve: AbortOnDropHandle<()>,
}

struct Shared {
    manifest: Manifest,
    asset: Vec<u8>,
    manifests: AtomicUsize,
    downloads: AtomicUsize,
    /// Set, a download stops halfway until [`FakeReleases::let_rest_through`].
    halfway: Option<Notify>,
}

impl FakeReleases {
    /// Serves `manifest` as the newest release, and `asset` as every version's app.
    pub async fn start(manifest: Manifest, asset: Vec<u8>) -> FakeReleases {
        Self::serve(manifest, asset, None).await
    }

    /// Like [`Self::start`], but each download stops halfway until [`Self::let_rest_through`].
    pub async fn holding_halfway(manifest: Manifest, asset: Vec<u8>) -> FakeReleases {
        Self::serve(manifest, asset, Some(Notify::new())).await
    }

    pub fn let_rest_through(&self) {
        if let Some(gate) = &self.shared.halfway {
            gate.notify_one();
        }
    }

    async fn serve(manifest: Manifest, asset: Vec<u8>, halfway: Option<Notify>) -> FakeReleases {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}/releases", listener.local_addr().expect("addr"));
        let shared = Arc::new(Shared {
            manifest,
            asset,
            manifests: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
            halfway,
        });
        let serve = tokio::spawn({
            let shared = shared.clone();
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(stream, shared.clone()));
                }
            }
        });
        FakeReleases {
            base,
            shared,
            _serve: AbortOnDropHandle::new(serve),
        }
    }

    pub fn feed(&self) -> ReleaseFeed {
        ReleaseFeed::at(&self.base).expect("a loopback URL")
    }

    pub fn manifests_served(&self) -> usize {
        self.shared.manifests.load(Ordering::SeqCst)
    }

    pub fn downloads(&self) -> usize {
        self.shared.downloads.load(Ordering::SeqCst)
    }
}

async fn serve(mut stream: tokio::net::TcpStream, shared: Arc<Shared>) {
    let Ok(head) = read_request(&mut stream).await else {
        return;
    };
    let path = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let _ = if path.ends_with(&format!("/latest/download/{MANIFEST}")) {
        shared.manifests.fetch_add(1, Ordering::SeqCst);
        let body = serde_json::to_vec(&shared.manifest).expect("a manifest serializes");
        respond(stream, 200, &body).await
    } else if path.ends_with(&format!("/{ASSET}")) {
        shared.downloads.fetch_add(1, Ordering::SeqCst);
        send_asset(stream, &shared).await
    } else {
        respond(stream, 404, b"{}").await
    };
}

async fn send_asset(mut stream: tokio::net::TcpStream, shared: &Shared) -> std::io::Result<()> {
    let asset = &shared.asset;
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        asset.len()
    );
    stream.write_all(head.as_bytes()).await?;
    let (first, rest) = asset.split_at(asset.len() / 2);
    stream.write_all(first).await?;
    stream.flush().await?;
    if let Some(gate) = &shared.halfway {
        gate.notified().await;
    }
    stream.write_all(rest).await?;
    stream.shutdown().await
}
