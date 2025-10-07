use ntied_server::Server;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize tracing subscriber with environment filter
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ntied_server=info,ntied_transport=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:39045".to_string());
    tracing::info!(?addr, "Starting server");
    let server = Server::new(&addr).await?;
    server.run().await?;
    Ok(())
}
