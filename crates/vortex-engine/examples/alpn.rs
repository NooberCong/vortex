//! Ad-hoc: what does the transport actually negotiate against real origins?
//!
//! `cargo run -p vortex-engine --example alpn`

use vortex_engine::transport::{ClientKey, Transport, TransportConfig};
use vortex_proto::RequestEnvelope;

#[tokio::main]
async fn main() {
    let transport = Transport::new(TransportConfig::default());
    for url in [
        "https://speed.cloudflare.com/__down?bytes=1024",
        "https://releases.ubuntu.com/24.04/",
        "https://www.google.com/",
    ] {
        let envelope = RequestEnvelope::new(url);
        let key = ClientKey {
            addr: None,
            force_h11: false,
        };
        match transport.request(&envelope, url, key, None) {
            Ok(req) => match req.send().await {
                Ok(r) => println!("{url}\n  -> {:?} {}", r.version(), r.status()),
                Err(e) => println!("{url}\n  -> error {e}"),
            },
            Err(e) => println!("{url}\n  -> build error {e}"),
        }
    }
}
