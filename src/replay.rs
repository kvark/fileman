use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ReplayCase {
    pub root: PathBuf,
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
    pub keys: Vec<ReplayKey>,
    pub asserts: Option<ReplayAsserts>,
}

#[derive(Deserialize)]
pub struct ReplayKey {
    pub key: String,
    pub modifiers: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct ReplayAsserts {
    pub fs: Option<FsAssert>,
    pub files: Option<Vec<FileAssert>>,
    pub snapshots: Option<Vec<SnapshotAssert>>,
}

#[derive(Deserialize)]
pub struct FsAssert {
    pub mode: Option<FsCheckMode>,
    pub entries: Vec<FsEntry>,
}

#[derive(Deserialize, Clone, Copy)]
pub enum FsCheckMode {
    Exact,
    Contains,
}

#[derive(Deserialize)]
pub struct FsEntry {
    pub path: String,
    pub kind: FsEntryKind,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsEntryKind {
    File,
    Dir,
}

#[derive(Deserialize)]
pub struct FileAssert {
    pub path: String,
    pub contains: Option<String>,
    pub equals: Option<String>,
}

#[derive(Deserialize)]
pub struct SnapshotAssert {
    pub path: PathBuf,
    pub expected: PathBuf,
    pub max_channel_diff: Option<u8>,
    pub max_pixel_fraction: Option<f32>,
}

pub fn load_replay_case(path: &PathBuf) -> Result<ReplayCase> {
    let text = std::fs::read_to_string(path)?;
    ron::from_str(&text).map_err(|err| anyhow::anyhow!("{err}"))
}
