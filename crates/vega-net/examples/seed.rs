//! A headless Vega node: bootstrap seed, relay, and mailbox.
//!
//! This is the piece that makes the network joinable. It holds no keys that can
//! read anything and stores only opaque ciphertext, so running one costs you a
//! port and some bandwidth and gives you no power over anyone's messages.
//!
//!   cargo run --release -p vega-net --example seed -- --port 15000
//!
//! Print its address on startup and hand that to clients as a bootstrap entry.

use std::time::Duration;
use vega_net::{NetEvent, Node, NodeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "seed=info,vega_net=info".into()),
        )
        .init();

    let port: u16 = std::env::args()
        .skip_while(|a| a != "--port")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(15000);

    let bootstrap = std::env::args()
        .skip_while(|a| a != "--bootstrap")
        .nth(1)
        .and_then(|a| a.parse().ok())
        .into_iter()
        .collect();

    // A seed listens on fixed, published ports — the whole point is that its
    // address can be written down and shipped in a client build.
    let config = NodeConfig::default()
        .with_listen(vec![
            format!("/ip6/::/udp/{port}/quic-v1").parse()?,
            format!("/ip4/0.0.0.0/udp/{port}/quic-v1").parse()?,
            format!("/ip6/::/tcp/{port}").parse()?,
            format!("/ip4/0.0.0.0/tcp/{port}").parse()?,
        ])
        .with_bootstrap(bootstrap);

    let (handle, mut events) = Node::spawn(config)?;
    let peer_id = handle.local_peer_id().await?;

    tracing::info!(%peer_id, "seed node starting");

    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                NetEvent::Listening(addr) => {
                    // The `/p2p/<id>` suffix is what makes this pasteable as a
                    // bootstrap entry, so print the complete form.
                    println!("listening: {addr}/p2p/{peer_id}");
                }
                NetEvent::PeerConnected(p) => tracing::info!(peer = %p, "peer connected"),
                NetEvent::PeerDisconnected(p) => tracing::info!(peer = %p, "peer left"),
                NetEvent::ExternalAddress(addr) => {
                    tracing::info!(%addr, "confirmed reachable from outside")
                }
                _ => {}
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    handle.shutdown().await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    Ok(())
}
