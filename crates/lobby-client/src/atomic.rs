//! Replace a file atomically: fill a temp file beside it, rename it into place, and remove it on
//! any failure, so a crash leaves the old file or the new one and at worst a temp file that the
//! media cache sweeps.

use std::{io, path::Path, path::PathBuf};

pub(crate) fn tmp_beside(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}.tmp-{:08x}", rand::random::<u32>()))
}

pub(crate) fn replace_blocking(
    dest: &Path,
    fill: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = tmp_beside(dest);
    let result = fill(&tmp).and_then(|()| std::fs::rename(&tmp, dest));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub(crate) async fn replace<E: From<io::Error>>(
    dest: &Path,
    fill: impl AsyncFnOnce(&Path) -> Result<(), E>,
) -> Result<(), E> {
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = tmp_beside(dest);
    let result = match fill(&tmp).await {
        Ok(()) => tokio::fs::rename(&tmp, dest).await.map_err(E::from),
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}
