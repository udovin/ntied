use ntied_transport::{PrivateKey, RelayNode};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ntied_server=info,ntied_transport=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let addr: std::net::SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:39045".to_string())
        .parse()?;

    let identity = PrivateKey::generate();
    let relay = RelayNode::bind(addr, identity).await?;

    tracing::info!(addr = %relay.local_addr()?, peer_id = %relay.peer_id(), "Relay server started");

    relay.run().await;

    Ok(())
}
