use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};

#[derive(Deserialize)]
pub struct ReplayCase {
    pub root: PathBuf,
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
    pub keys: Vec<ReplayKey>,
    #[serde(default)]
    pub asserts: ReplayAsserts,
}

#[derive(Deserialize)]
pub struct ReplayKey {
    pub key: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_option")]
    pub modifiers: Vec<String>,
}

#[derive(Deserialize, Default)]
pub struct ReplayAsserts {
    pub fs: Option<FsAssert>,
    #[serde(default, deserialize_with = "deserialize_vec_or_option")]
    pub files: Vec<FileAssert>,
    #[serde(default, deserialize_with = "deserialize_vec_or_option")]
    pub snapshots: Vec<SnapshotAssert>,
}

#[derive(Deserialize)]
pub struct FsAssert {
    #[serde(default)]
    pub mode: FsCheckMode,
    pub entries: Vec<FsEntry>,
}

#[derive(Deserialize, Clone, Copy, Default)]
pub enum FsCheckMode {
    #[default]
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
    #[serde(default = "default_max_channel_diff")]
    pub max_channel_diff: u8,
    #[serde(default = "default_max_pixel_fraction")]
    pub max_pixel_fraction: f32,
}

pub fn load_replay_case(path: &PathBuf) -> Result<ReplayCase> {
    let text = std::fs::read_to_string(path)?;
    ron::from_str(&text).map_err(|err| anyhow::anyhow!("{err}"))
}

fn default_max_channel_diff() -> u8 {
    4
}

fn default_max_pixel_fraction() -> f32 {
    0.001
}

fn deserialize_vec_or_option<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MaybeVec<T> {
        Vec(Vec<T>),
        Option(Option<Vec<T>>),
    }

    match MaybeVec::deserialize(deserializer)? {
        MaybeVec::Vec(items) => Ok(items),
        MaybeVec::Option(items) => Ok(items.unwrap_or_default()),
    }
}
