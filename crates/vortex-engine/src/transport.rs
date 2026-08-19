//! Transport (02 §1). Buy the transport, build the scheduler.
//!
//! h1.1 and h2 go through `reqwest`/`hyper` — the stable, boring, correct path. HTTP/3
//! sits behind the same [`Transport`] surface and is **never load-bearing**: any handshake
//! failure or throughput regression demotes the origin to h2 for 24 hours, silently, and a
//! single setting disables it globally (see [`crate::policy`]).
//!
//! Two disciplines matter more than anything clever here:
//!
//! * `Accept-Encoding: identity`, always. Content coding on a ranged request destroys byte
//!   offsets, and some CDNs will happily gzip a 206.
//! * Every request carries the captured envelope verbatim. Divergence from what the browser
//!   sent is the number one cause of a 403 on handoff.

use crate::error::{EngineError, Result};
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use url::Url;
use vortex_proto::{Protocol, RequestEnvelope};

/// Headers the transport owns. A replayed value for any of these would be wrong or
/// dangerous, so the envelope's version is dropped.
const RESERVED: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
    "content-length",
    "accept-encoding",
    "range",
    "if-range",
    "if-none-match",
    "if-modified-since",
];

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub connect_timeout: Duration,
    /// Per-read, not per-request: a 5 GB body must not be killed by a total deadline.
    pub read_timeout: Duration,
    pub enable_h3: bool,
    pub user_agent: String,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            enable_h3: true,
            user_agent: format!("Vortex/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientKey {
    /// Pins this client to one resolved address, which is how multi-IP fanout lands
    /// workers on different edge machines.
    pub addr: Option<SocketAddr>,
    pub force_h11: bool,
}

/// A pool of clients keyed by `(origin, alpn, resolved_ip)`.
pub struct Transport {
    cfg: TransportConfig,
    clients: Mutex<HashMap<(String, ClientKey), reqwest::Client>>,
}

impl Transport {
    pub fn new(cfg: TransportConfig) -> Self {
        Self {
            cfg,
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &TransportConfig {
        &self.cfg
    }

    pub fn client(&self, host: &str, key: ClientKey) -> Result<reqwest::Client> {
        if let Some(client) = self.clients.lock().get(&(host.to_owned(), key)) {
            return Ok(client.clone());
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(self.cfg.connect_timeout)
            .read_timeout(self.cfg.read_timeout)
            .user_agent(self.cfg.user_agent.clone())
            .pool_max_idle_per_host(2)
            // TLS 1.3 session resumption is on by default in rustls: on a 200 ms path, one
            // saved round trip per connection across 16 connections is not a rounding error.
            .redirect(reqwest::redirect::Policy::limited(10));
        if key.force_h11 {
            builder = builder.http1_only();
        }
        if let Some(addr) = key.addr {
            builder = builder.resolve(host, addr);
        }
        let client = builder
            .build()
            .map_err(|e| EngineError::with_source(crate::error::Class::Local, "TLS setup", e))?;
        self.clients
            .lock()
            .insert((host.to_owned(), key), client.clone());
        Ok(client)
    }

    /// Builds a request that replays the envelope verbatim, with `range` layered on top.
    pub fn request(
        &self,
        envelope: &RequestEnvelope,
        url: &str,
        key: ClientKey,
        range: Option<ByteRange>,
    ) -> Result<reqwest::RequestBuilder> {
        let parsed = Url::parse(url)
            .map_err(|e| EngineError::fatal(format!("that does not look like a URL: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| EngineError::fatal("that URL has no host"))?
            .to_owned();

        let method = reqwest::Method::from_bytes(envelope.method.as_bytes())
            .map_err(|_| EngineError::fatal("unsupported HTTP method"))?;
        let client = self.client(&host, key)?;
        let mut req = client.request(method, parsed);

        let mut headers = HeaderMap::new();
        for (name, value) in &envelope.headers {
            if RESERVED.iter().any(|r| name.eq_ignore_ascii_case(r)) {
                continue;
            }
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.append(n, v);
            }
        }
        if let Some(cookies) = &envelope.cookies {
            if !cookies.is_empty() {
                if let Ok(v) = HeaderValue::from_str(cookies) {
                    headers.insert(reqwest::header::COOKIE, v);
                }
            }
        }
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        if let Some(range) = range {
            headers.insert(
                reqwest::header::RANGE,
                HeaderValue::from_str(&range.header()).expect("range headers are ASCII"),
            );
        }
        req = req.headers(headers);

        if let Some(body) = &envelope.body_base64 {
            use base64::Engine as _;
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(body) {
                req = req.body(bytes);
            }
        }
        Ok(req)
    }
}

/// An HTTP byte range, inclusive on both ends as the header wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// `None` = open-ended, i.e. `bytes=start-`.
    pub end: Option<u64>,
}

impl ByteRange {
    pub fn new(start: u64, end_exclusive: Option<u64>) -> Self {
        Self {
            start,
            end: end_exclusive.map(|e| e.saturating_sub(1)),
        }
    }

    pub fn header(&self) -> String {
        match self.end {
            Some(end) => format!("bytes={}-{}", self.start, end),
            None => format!("bytes={}-", self.start),
        }
    }
}

/// What `Content-Range: bytes a-b/c` actually said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.trim().split_once('-')?;
    Some(ContentRange {
        start: start.trim().parse().ok()?,
        end: end.trim().parse().ok()?,
        total: total.trim().parse().ok(),
    })
}

/// Which protocol a response actually arrived on, for the expanded row.
pub fn protocol_of(version: reqwest::Version) -> Protocol {
    match version {
        reqwest::Version::HTTP_3 => Protocol::H3,
        reqwest::Version::HTTP_2 => Protocol::H2,
        _ => Protocol::Http11,
    }
}

/// Resolve a host to its full A/AAAA set. Workers are then distributed across **distinct
/// addresses**, which on an anycast CDN frequently lands them on different edge machines
/// and raises the aggregate ceiling above what one socket to one PoP can deliver. It is
/// also free resilience: one bad edge node degrades one worker instead of the job.
/// The addresses worth fanning workers across.
///
/// `reached` is the address the probe actually connected on, and it is what keeps this
/// honest. A name on a CDN routinely resolves to both an A and a AAAA record, and a great
/// many real machines have IPv6 that is configured but not reachable — a tunnel that never
/// came up, a router that advertises a prefix it cannot route. `curl` and every browser
/// survive that through Happy Eyeballs: they race the families and use whichever answers.
///
/// Fanout has no such race. It pins one worker per address, so on such a machine half the
/// workers would sit in a connect timeout, retry, back off, and drag the whole transfer
/// below what a single stream would have managed. Measured against a real CDN before this:
/// 40.8 s where `curl` took 6.3 s, for the same 100 MB.
///
/// The probe already answered the question — it connects unpinned, so the resolver and the
/// stack between them picked a family that works. Everything in another family is a guess,
/// and a guess is not another edge machine.
pub async fn resolve(url: &str, reached: Option<SocketAddr>) -> Vec<SocketAddr> {
    let Ok(parsed) = Url::parse(url) else {
        return Vec::new();
    };
    let Some(host) = parsed.host_str().map(str::to_owned) else {
        return Vec::new();
    };
    let port = parsed
        .port_or_known_default()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });

    let Ok(addrs) = tokio::net::lookup_host(format!("{host}:{port}")).await else {
        return reached.into_iter().collect();
    };
    fan_out(addrs, reached)
}

/// The filtering half of [`resolve`], without the DNS.
fn fan_out(addrs: impl Iterator<Item = SocketAddr>, reached: Option<SocketAddr>) -> Vec<SocketAddr> {
    let mut seen: Vec<SocketAddr> = Vec::new();
    let mut ips: Vec<IpAddr> = Vec::new();
    for addr in addrs {
        // Only the family the probe proved. With no proof, every address stays: guessing a
        // family would be the same mistake in the other direction.
        if reached.is_some_and(|r| r.is_ipv4() != addr.is_ipv4()) {
            continue;
        }
        if !ips.contains(&addr.ip()) {
            ips.push(addr.ip());
            seen.push(addr);
        }
    }
    // The one address known to work leads, and is present even if resolution has since
    // changed its mind about which addresses exist.
    if let Some(reached) = reached {
        seen.retain(|a| a.ip() != reached.ip());
        seen.insert(0, reached);
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("a literal socket address")
    }

    #[test]
    fn fanout_never_spreads_onto_a_family_the_probe_did_not_prove() {
        // The machine this was found on: a name with both records, IPv6 configured and
        // unroutable. Fanning a worker onto the v6 address costs a connect timeout, a
        // retry and a backoff, per worker, and lost to a single `curl` stream by 6x.
        let dns = [
            addr("[2a01:4ff:2ef::fa57:1]:443"),
            addr("5.223.7.195:443"),
            addr("5.223.7.196:443"),
        ];
        let out = fan_out(dns.into_iter(), Some(addr("5.223.7.195:443")));
        assert_eq!(out, vec![addr("5.223.7.195:443"), addr("5.223.7.196:443")]);
    }

    #[test]
    fn the_address_that_answered_goes_first_even_if_dns_has_moved_on() {
        let out = fan_out(
            [addr("203.0.113.9:80")].into_iter(),
            Some(addr("203.0.113.1:80")),
        );
        assert_eq!(out, vec![addr("203.0.113.1:80"), addr("203.0.113.9:80")]);
    }

    #[test]
    fn with_nothing_proven_every_address_survives_deduplicated() {
        let dns = [
            addr("203.0.113.1:80"),
            addr("203.0.113.1:80"),
            addr("[2001:db8::1]:80"),
        ];
        let out = fan_out(dns.into_iter(), None);
        assert_eq!(out, vec![addr("203.0.113.1:80"), addr("[2001:db8::1]:80")]);
    }

    #[test]
    fn range_headers_are_inclusive_on_both_ends() {
        assert_eq!(ByteRange::new(0, Some(16)).header(), "bytes=0-15");
        assert_eq!(ByteRange::new(1024, None).header(), "bytes=1024-");
    }

    #[test]
    fn content_range_parses_including_the_unknown_total() {
        assert_eq!(
            parse_content_range("bytes 0-15/4096"),
            Some(ContentRange {
                start: 0,
                end: 15,
                total: Some(4096)
            })
        );
        assert_eq!(
            parse_content_range("bytes 0-0/*"),
            Some(ContentRange {
                start: 0,
                end: 0,
                total: None
            })
        );
        assert_eq!(parse_content_range("pages 1-2/3"), None);
    }

    #[test]
    fn reserved_headers_are_never_replayed_from_the_envelope() {
        let transport = Transport::new(TransportConfig::default());
        let mut env = RequestEnvelope::new("https://example.com/a.iso");
        env.set_header("User-Agent", "Mozilla/5.0");
        env.set_header("Referer", "https://example.com/page");
        env.set_header("Accept-Encoding", "gzip, br");
        env.set_header("Content-Length", "999");
        env.cookies = Some("session=abc".into());

        let req = transport
            .request(
                &env,
                "https://example.com/a.iso",
                ClientKey {
                    addr: None,
                    force_h11: false,
                },
                Some(ByteRange::new(0, Some(16))),
            )
            .unwrap()
            .build()
            .unwrap();

        let headers = req.headers();
        assert_eq!(headers[reqwest::header::ACCEPT_ENCODING], "identity");
        assert_eq!(headers[reqwest::header::RANGE], "bytes=0-15");
        assert_eq!(headers[reqwest::header::REFERER], "https://example.com/page");
        assert_eq!(headers[reqwest::header::COOKIE], "session=abc");
        assert!(headers.get(reqwest::header::CONTENT_LENGTH).is_none());
    }
}
