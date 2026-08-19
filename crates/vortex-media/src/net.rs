//! One-shot fetches through the engine's transport.
//!
//! Manifests, keys and segments are all small enough to want whole, and all three must be
//! fetched with the captured envelope — the same cookies, the same `Referer`, the same
//! `User-Agent` the page used. A key server that sees a different `Referer` returns a 403,
//! and the resulting file is a perfectly-downloaded pile of noise.

use crate::Resource;
use vortex_engine::error::{Class, EngineError, Result};
use vortex_engine::job::Engine;
use vortex_engine::transport::{ByteRange, ClientKey};
use vortex_proto::RequestEnvelope;

/// Manifests are text and small. A "manifest" larger than this is a redirect to something
/// that is not a manifest.
pub const MAX_MANIFEST: usize = 16 * 1024 * 1024;
/// A single segment. Ten seconds of 4K is a few tens of megabytes; a gigabyte is a lie.
pub const MAX_SEGMENT: usize = 256 * 1024 * 1024;

pub struct Fetched {
    pub body: Vec<u8>,
    pub final_url: String,
    pub content_type: Option<String>,
}

/// Fetches one resource once. Retries belong to the caller, which knows whether the thing
/// being fetched is worth retrying and for how long.
pub async fn get(
    engine: &Engine,
    envelope: &RequestEnvelope,
    resource: &Resource,
    limit: usize,
) -> Result<Fetched> {
    let range = resource
        .range
        .map(|(offset, length)| ByteRange::new(offset, Some(offset + length)));
    let request = engine.transport.request(
        envelope,
        &resource.url,
        ClientKey {
            addr: None,
            force_h11: false,
        },
        range,
    )?;

    let response = request
        .send()
        .await
        .map_err(|e| EngineError::with_source(Class::Transient, "the request failed", e))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        // "Previously worked" is true by construction here: the browser fetched this
        // manifest moments ago, so a 403 means the signature died, not that we were never
        // allowed in.
        return Err(EngineError::from_status(status, true));
    }
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_owned());

    let body = response
        .bytes()
        .await
        .map_err(|e| EngineError::with_source(Class::Transient, "the body was cut short", e))?;
    if body.len() > limit {
        return Err(EngineError::integrity(format!(
            "{} returned {} bytes where at most {limit} were expected",
            resource.url,
            body.len()
        )));
    }
    engine.limiter.consume(body.len() as u64).await;

    Ok(Fetched {
        body: body.to_vec(),
        final_url,
        content_type,
    })
}

/// Fetches a manifest and decodes it as text.
pub async fn get_text(
    engine: &Engine,
    envelope: &RequestEnvelope,
    url: &str,
) -> Result<(String, Fetched)> {
    let fetched = get(engine, envelope, &Resource::whole(url), MAX_MANIFEST).await?;
    let text = String::from_utf8_lossy(&fetched.body).into_owned();
    Ok((text, fetched))
}
