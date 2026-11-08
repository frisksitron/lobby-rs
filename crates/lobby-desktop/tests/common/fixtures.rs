#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn big_file(dir: &Path, name: &str, mib: u64) -> PathBuf {
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(mib * 1024 * 1024).unwrap();
    path
}
