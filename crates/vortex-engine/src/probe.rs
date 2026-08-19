//! Probe (02 §2). Probing is where correctness is won or lost, because servers lie.
//!
//! ```text
//! 1. GET with a tiny Range   ← not HEAD. Many CDNs mishandle or reject HEAD, and HEAD
//!                              tells you nothing about range behaviour.
//! 2. Follow redirects, record the FINAL url — that is the one workers use.
//! 3. Classify: 206+total → PARALLEL · 206+* / 200 / 416 → SINGLE
//! 4. Capture validators: ETag (strong preferred), Last-Modified, sizes, checksums
//! 5. LIAR CHECK: a second probe in the middle of the file
//! ```
//!
//! The probe asks for `bytes=0-15` rather than `bytes=0-0` so the liar check has something
//! to compare against. It costs fifteen extra bytes and removes an entire class of silent
//! corruption.

use crate::error::{Class, EngineError, Result};
use crate::integrity::{self, Expected};
use crate::naming;
use crate::transport::{self, ByteRange, ClientKey, Transport};
use std::net::SocketAddr;
use vortex_proto::{Category, Protocol, RequestEnvelope, TransferMode};

/// How many bytes the probe reads at each end. Enough to compare, small enough to be free.
const SAMPLE: u64 = 16;
/// Below this a parallel transfer cannot pay for its own handshakes (02 §3).
pub const PARALLEL_FLOOR: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Probe {
    pub url: String,
    /// Post-redirect. This is the URL workers use.
    pub final_url: String,
    pub host: String,
    pub total: Option<u64>,
    pub ranges: bool,
    pub mode: TransferMode,
    pub etag: Option<String>,
    /// A `W/` tag is recorded but is unusable for `If-Range` across CDN nodes on its own.
    pub weak_etag: bool,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub filename: String,
    pub category: Category,
    pub digest: Option<Expected>,
    pub protocol: Protocol,
    pub addresses: Vec<SocketAddr>,
    /// Set when the liar check found range support it could not trust.
    pub ranges_distrusted: bool,
}

impl Probe {
    /// Resume needs a validator the server will honour. A weak ETag alone is not one.
    pub fn resumable(&self) -> bool {
        self.ranges && self.total.is_some()
    }

    pub fn if_range_value(&self) -> Option<String> {
        match (&self.etag, self.weak_etag, &self.last_modified) {
            (Some(tag), false, _) => Some(tag.clone()),
            (_, _, Some(date)) => Some(date.clone()),
            _ => None,
        }
    }
}

/// A probe is one round trip against a server that may simply be having a bad moment. A
/// 503 on first contact is not a reason to make the user press retry.
const PROBE_ATTEMPTS: u32 = 3;

pub async fn probe(transport: &Transport, envelope: &RequestEnvelope) -> Result<Probe> {
    let mut last = None;
    for attempt in 0..PROBE_ATTEMPTS {
        match probe_once(transport, envelope).await {
            Ok(probe) => return Ok(probe),
            Err(e) if e.class == Class::Transient => {
                tokio::time::sleep(std::time::Duration::from_millis(150 << attempt)).await;
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| EngineError::transient("the server did not answer")))
}

async fn probe_once(transport: &Transport, envelope: &RequestEnvelope) -> Result<Probe> {
    let url = envelope.effective_url().to_owned();
    let key = ClientKey {
        addr: None,
        force_h11: false,
    };

    let response = send(transport.request(envelope, &url, key, Some(ByteRange::new(0, Some(SAMPLE))))).await?;

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        // A 401/403 here is classified renewable rather than fatal. The envelope came from
        // a browser that had this URL working moments ago, so the right first move is to
        // ask for a fresh one — not to tell the user their download failed. If no renewal
        // arrives, the caller degrades to "Reopen the page to continue" (03 §Handoff).
        return Err(EngineError::from_status(status, true));
    }

    let protocol = transport::protocol_of(response.version());
    // Which address this actually landed on, read before the response is consumed. This is
    // what stops fanout from spreading workers onto an address family the machine cannot
    // reach — see [`transport::resolve`].
    let reached = response.remote_addr();
    let final_url = response.url().to_string();
    let host = response
        .url()
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Everything the headers have to say, read out before the body is touched.
    let headers = response.headers().clone();
    let header = move |name: &str| -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned())
    };

    let content_range = header("content-range").and_then(|v| transport::parse_content_range(&v));
    let content_length = header("content-length").and_then(|v| v.parse::<u64>().ok());
    let etag_raw = header("etag");
    let weak_etag = etag_raw.as_deref().is_some_and(|t| t.starts_with("W/"));
    let content_type = header("content-type");
    let disposition = header("content-disposition");
    let last_modified = header("last-modified");
    let digest = integrity::from_headers(&header);

    let (total, ranges) = match (status, &content_range) {
        // 206 + `bytes 0-15/N` → ranges work and the size is known.
        (206, Some(cr)) if cr.total.is_some() => (cr.total, true),
        // 206 + `bytes 0-15/*` → ranges work, size unknown. Streaming single.
        (206, Some(_)) => (None, true),
        // 206 without a usable Content-Range is a server contradicting itself.
        (206, None) => (content_length, false),
        // 200 means the server ignored Range. Its Content-Length is the whole file.
        _ => (content_length, false),
    };

    let sample = if status == 206 {
        read_sample(response).await
    } else {
        // The body is the entire file; do not drain it during a probe.
        drop(response);
        Vec::new()
    };

    // Naming, best source first. The browser's own resolved filename beats a guess from
    // the URL: `downloads.onDeterminingFilename` has already applied everything the
    // browser knows, and a URL basename is often `file`, `download`, or a uuid (03 §2).
    let filename = disposition
        .as_deref()
        .and_then(naming::parse_disposition)
        .map(|name| naming::sanitize(&name))
        .or_else(|| envelope.filename_hint.as_deref().map(naming::sanitize))
        .or_else(|| naming::from_url(&final_url))
        .or_else(|| envelope.page_title.as_deref().map(naming::from_page_title))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "download".to_owned());
    let category = naming::categorize(&filename, content_type.as_deref());

    let mut probe = Probe {
        url,
        final_url,
        host,
        total,
        ranges,
        mode: TransferMode::Single,
        etag: etag_raw,
        weak_etag,
        last_modified,
        content_type,
        filename,
        category,
        digest,
        protocol,
        addresses: Vec::new(),
        ranges_distrusted: false,
    };

    // The liar check. Catches the class of proxy that returns 206 with the wrong body,
    // which otherwise silently corrupts the file. One round trip.
    if let (true, Some(total)) = (probe.ranges, probe.total) {
        if total >= SAMPLE * 4 {
            match verify_ranges(transport, envelope, &probe, total, &sample).await {
                Ok(true) => {}
                Ok(false) => {
                    probe.ranges = false;
                    probe.ranges_distrusted = true;
                }
                Err(e) if e.class == Class::Integrity => return Err(e),
                // A transient failure on the second probe is not evidence of lying; it is
                // just a flaky moment. Fall back to one stream rather than guess.
                Err(_) => {
                    probe.ranges = false;
                    probe.ranges_distrusted = true;
                }
            }
        }
    }

    probe.mode = match (probe.ranges, probe.total) {
        (true, Some(total)) if total >= PARALLEL_FLOOR => TransferMode::Parallel,
        _ => TransferMode::Single,
    };
    probe.addresses = transport::resolve(&probe.final_url, reached).await;

    Ok(probe)
}

/// Returns `Ok(false)` when ranges look untrustworthy but not provably wrong, and an
/// `Integrity` error when the server contradicted itself outright.
async fn verify_ranges(
    transport: &Transport,
    envelope: &RequestEnvelope,
    probe: &Probe,
    total: u64,
    head_sample: &[u8],
) -> Result<bool> {
    let midpoint = (total / 2) & !(SAMPLE - 1);
    let want = ByteRange::new(midpoint, Some(midpoint + SAMPLE));

    let response = send(transport.request(
        envelope,
        &probe.final_url,
        ClientKey {
            addr: None,
            force_h11: false,
        },
        Some(want),
    ))
    .await?;

    if response.status().as_u16() != 206 {
        // It advertised range support on the first request and ignored it on the second.
        return Ok(false);
    }
    let claimed = response
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(transport::parse_content_range);
    let Some(claimed) = claimed else {
        return Ok(false);
    };
    if claimed.start != midpoint {
        return Err(EngineError::integrity(format!(
            "the server answered a request for byte {midpoint} with byte {}",
            claimed.start
        )));
    }
    if let Some(claimed_total) = claimed.total {
        if claimed_total != total {
            return Err(EngineError::integrity(
                "the server reported two different sizes for the same file",
            ));
        }
    }

    let sample = read_sample(response).await;
    if sample.len() < head_sample.len() {
        return Ok(false);
    }
    // Identical bytes are not proof of a lie — a sparse or zero-filled file is legitimate.
    // Distrust ranges and use one stream, which cannot be corrupted this way, rather than
    // failing a download that may be perfectly fine.
    Ok(sample != head_sample)
}

async fn read_sample(response: reqwest::Response) -> Vec<u8> {
    match response.bytes().await {
        Ok(b) => b.to_vec(),
        Err(_) => Vec::new(),
    }
}

async fn send(builder: Result<reqwest::RequestBuilder>) -> Result<reqwest::Response> {
    Ok(builder?.send().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_weak_etag_never_becomes_an_if_range_value_on_its_own() {
        let mut p = Probe {
            url: "u".into(),
            final_url: "u".into(),
            host: "h".into(),
            total: Some(100),
            ranges: true,
            mode: TransferMode::Parallel,
            etag: Some("W/\"abc\"".into()),
            weak_etag: true,
            last_modified: None,
            content_type: None,
            filename: "f".into(),
            category: Category::Other,
            digest: None,
            protocol: Protocol::H2,
            addresses: vec![],
            ranges_distrusted: false,
        };
        assert_eq!(p.if_range_value(), None);
        p.last_modified = Some("Tue, 1 Jan 2030 00:00:00 GMT".into());
        assert_eq!(
            p.if_range_value().as_deref(),
            Some("Tue, 1 Jan 2030 00:00:00 GMT")
        );
        p.etag = Some("\"abc\"".into());
        p.weak_etag = false;
        assert_eq!(p.if_range_value().as_deref(), Some("\"abc\""));
    }
}
