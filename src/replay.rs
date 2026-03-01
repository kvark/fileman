use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ReplayCase {
    pub root: PathBuf,
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
    pub keys: Vec<ReplayKey>,
}

#[derive(Deserialize)]
pub struct ReplayKey {
    pub key: String,
    pub modifiers: Option<Vec<String>>,
}

pub fn load_replay_case(path: &PathBuf) -> Result<ReplayCase> {
    let text = std::fs::read_to_string(path)?;
    ron::from_str(&text).map_err(|err| anyhow::anyhow!("{err}"))
}
