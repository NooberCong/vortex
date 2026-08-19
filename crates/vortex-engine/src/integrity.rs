//! Integrity (02 §7).
//!
//! Always: bytes written == `Content-Length`. When the server offers a checksum, verify it
//! and **surface the result** — a verified download gets a checkmark with the algorithm
//! named, an unverifiable one gets nothing. Never a fake green tick.
//!
//! Single-stream transfers hash incrementally as bytes arrive, so verification is free.
//! Parallel transfers cannot: sixteen workers deliver out of order, so the file is read
//! back once at the end. That read is sequential and usually still in page cache.

use crate::error::{EngineError, Result};
use md5::Md5;
use sha2::{Digest as _, Sha256, Sha512};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algo {
    Sha256,
    Sha512,
    Md5,
    Crc32,
}

impl Algo {
    /// The name the UI shows on hover.
    pub fn label(self) -> &'static str {
        match self {
            Algo::Sha256 => "SHA-256",
            Algo::Sha512 => "SHA-512",
            Algo::Md5 => "MD5",
            Algo::Crc32 => "CRC32",
        }
    }
}

/// A checksum the server claimed, normalised to lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub algo: Algo,
    pub hex: String,
}

/// Incremental hasher. Cheap to feed, so single-stream jobs always do.
pub enum Hasher {
    Sha256(Sha256),
    Sha512(Sha512),
    Md5(Md5),
    Crc32(crc32fast::Hasher),
}

impl Hasher {
    pub fn new(algo: Algo) -> Self {
        match algo {
            Algo::Sha256 => Hasher::Sha256(Sha256::new()),
            Algo::Sha512 => Hasher::Sha512(Sha512::new()),
            Algo::Md5 => Hasher::Md5(Md5::new()),
            Algo::Crc32 => Hasher::Crc32(crc32fast::Hasher::new()),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        match self {
            Hasher::Sha256(h) => h.update(bytes),
            Hasher::Sha512(h) => h.update(bytes),
            Hasher::Md5(h) => h.update(bytes),
            Hasher::Crc32(h) => h.update(bytes),
        }
    }

    pub fn finish(self) -> String {
        match self {
            Hasher::Sha256(h) => hex::encode(h.finalize()),
            Hasher::Sha512(h) => hex::encode(h.finalize()),
            Hasher::Md5(h) => hex::encode(h.finalize()),
            Hasher::Crc32(h) => hex::encode(h.finalize().to_be_bytes()),
        }
    }
}

/// Reads the finished file once and returns its digest.
pub async fn hash_file(path: &Path, algo: Algo) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Hasher::new(algo);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish())
}

/// Finds the strongest checksum the response offered, across the several spellings the
/// large object stores actually use.
pub fn from_headers(get: impl Fn(&str) -> Option<String>) -> Option<Expected> {
    // RFC 9530 first — `sha-256=:base64:` — then its RFC 3230 predecessor.
    for header in ["repr-digest", "digest", "content-digest"] {
        if let Some(value) = get(header) {
            if let Some(found) = parse_digest_field(&value) {
                return Some(found);
            }
        }
    }
    for (header, algo) in [
        ("x-amz-checksum-sha256", Algo::Sha256),
        ("x-amz-checksum-crc32", Algo::Crc32),
    ] {
        if let Some(value) = get(header) {
            if let Some(hex) = decode_to_hex(value.trim()) {
                return Some(Expected { algo, hex });
            }
        }
    }
    // `x-goog-hash: crc32c=…,md5=…`. crc32c is Castagnoli, which nothing here computes,
    // so only the md5 member is usable.
    if let Some(value) = get("x-goog-hash") {
        for part in value.split(',') {
            if let Some(b64) = part.trim().strip_prefix("md5=") {
                if let Some(hex) = decode_to_hex(b64.trim()) {
                    return Some(Expected {
                        algo: Algo::Md5,
                        hex,
                    });
                }
            }
        }
    }
    if let Some(value) = get("content-md5") {
        if let Some(hex) = decode_to_hex(value.trim()) {
            return Some(Expected {
                algo: Algo::Md5,
                hex,
            });
        }
    }
    None
}

fn parse_digest_field(value: &str) -> Option<Expected> {
    for member in value.split(',') {
        let (name, raw) = member.trim().split_once('=')?;
        let algo = match name.trim().to_ascii_lowercase().as_str() {
            "sha-256" | "sha256" => Algo::Sha256,
            "sha-512" | "sha512" => Algo::Sha512,
            "md5" => Algo::Md5,
            _ => continue,
        };
        // Structured-field byte sequences are wrapped in colons.
        let raw = raw.trim().trim_matches(':');
        if let Some(hex) = decode_to_hex(raw) {
            return Some(Expected { algo, hex });
        }
    }
    None
}

/// Accepts either hex or base64, because servers use both and sometimes disagree with
/// their own documentation.
fn decode_to_hex(raw: &str) -> Option<String> {
    let raw = raw.trim_matches('"');
    if !raw.is_empty()
        && raw.len() % 2 == 0
        && raw.chars().all(|c| c.is_ascii_hexdigit())
        && raw.len() >= 8
    {
        return Some(raw.to_ascii_lowercase());
    }
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .ok()
        .filter(|b| !b.is_empty())
        .map(hex::encode)
}

/// Compares a computed digest against what the server claimed.
pub fn check(expected: &Expected, actual_hex: &str) -> Result<()> {
    if expected.hex.eq_ignore_ascii_case(actual_hex) {
        Ok(())
    } else {
        Err(EngineError::integrity(format!(
            "the {} checksum does not match what the server declared",
            expected.algo.label()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn getter(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn rfc_9530_repr_digest_is_understood() {
        // sha-256 of "abc"
        let b64 = "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
        let found = from_headers(getter(&[("repr-digest", &format!("sha-256=:{b64}:"))])).unwrap();
        assert_eq!(found.algo, Algo::Sha256);
        assert_eq!(
            found.hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn amazon_and_google_spellings_are_understood() {
        let amz = from_headers(getter(&[(
            "x-amz-checksum-sha256",
            "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=",
        )]))
        .unwrap();
        assert_eq!(amz.algo, Algo::Sha256);

        let goog = from_headers(getter(&[(
            "x-goog-hash",
            "crc32c=AAAAAA==,md5=kAFQmDzST7DWlj99KOF/cg==",
        )]))
        .unwrap();
        assert_eq!(goog.algo, Algo::Md5);
    }

    #[test]
    fn a_missing_checksum_is_not_an_error_it_is_just_nothing() {
        assert!(from_headers(getter(&[("etag", "\"abc\"")])).is_none());
    }

    #[test]
    fn mismatch_is_an_integrity_failure_not_a_retry() {
        let expected = Expected {
            algo: Algo::Sha256,
            hex: "aa".repeat(32),
        };
        let err = check(&expected, &"bb".repeat(32)).unwrap_err();
        assert_eq!(err.class, crate::error::Class::Integrity);
    }

    #[tokio::test]
    async fn hashing_a_file_matches_the_incremental_hasher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        tokio::fs::write(&path, b"abc").await.unwrap();

        let mut inc = Hasher::new(Algo::Sha256);
        inc.update(b"abc");
        assert_eq!(hash_file(&path, Algo::Sha256).await.unwrap(), inc.finish());
    }
}
