use std::net::SocketAddr;

use clap::Parser;
use ntied_transport::PrivateKey;
use ntied_transport::relay::RelayNode;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Ntied relay server.
#[derive(Debug, Parser)]
#[command(name = "ntied-server", version, about, long_about = None)]
struct Args {
    /// Address to bind the UDP transport socket to.
    #[arg(short, long, default_value = "0.0.0.0:39045")]
    bind: SocketAddr,

    /// Publish this relay in the public DHT registry (`H_relays`) so
    /// fresh peers can discover it via `Node::lookup_relays`.
    /// Off by default — opt-in for relays that are intentionally public.
    #[arg(long, default_value_t = false)]
    publish_dht: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ntied_transport=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();

    let identity = PrivateKey::generate();
    let relay = RelayNode::bind(args.bind, identity).await?;

    if args.publish_dht {
        relay.enable_public_relay().await?;
        tracing::info!("Publishing self in DHT (`H_relays` registry)");
    } else {
        tracing::info!(
            "DHT publication disabled (pass --publish-dht to register in `H_relays`)"
        );
    }

    tracing::info!(
        addr = %relay.local_addr()?,
        peer_id = %relay.peer_id(),
        publish_dht = args.publish_dht,
        "Relay server started",
    );

    relay.run().await?;

    Ok(())
}
