//! Identifiers, API keys and time helpers.

use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

pub fn random_token(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

pub fn team_id() -> String {
    format!("tm_{}", random_token(12))
}

pub fn run_id() -> String {
    format!("run_{}", random_token(12))
}

pub fn worker_id() -> String {
    format!("wk_{}", random_token(8))
}

pub fn submission_id(team_id: &str, seq: u64) -> String {
    format!("sub_{}_{:04}", team_id, seq)
}

/// A new API key. Shown once; only its hash is stored.
pub fn api_key() -> String {
    format!("pb_live_{}", random_token(40))
}

pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn key_prefix(key: &str) -> String {
    key.chars().take(12).collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

pub fn now_str() -> String {
    fmt_time(&Utc::now())
}

pub fn fmt_time(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}
