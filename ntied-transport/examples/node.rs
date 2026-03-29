use std::net::SocketAddr;

use ntied_transport::{Node, NodeConfig, PrivateKey};

/// Usage:
///   # Start infrastructure (relay + registry), first seed:
///   cargo run --example node -- --bind 127.0.0.1:5000 --relay --registry
///
///   # Start second infra node:
///   cargo run --example node -- --bind 127.0.0.1:5001 --relay --registry --bootstrap 127.0.0.1:5000
///
///   # Start peer A:
///   cargo run --example node -- --bind 127.0.0.1:0 --bootstrap 127.0.0.1:5000 --name Alice
///
///   # Start peer B:
///   cargo run --example node -- --bind 127.0.0.1:0 --bootstrap 127.0.0.1:5000 --name Bob --connect <Alice_PeerId>
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();

    let mut bind_addr: SocketAddr = "127.0.0.1:0".parse()?;
    let mut bootstrap: Vec<SocketAddr> = Vec::new();
    let mut relay = false;
    let mut registry = false;
    let mut name = String::from("node");
    let mut connect_to: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind_addr = args[i].parse()?;
            }
            "--bootstrap" => {
                i += 1;
                bootstrap.push(args[i].parse()?);
            }
            "--relay" => relay = true,
            "--registry" => registry = true,
            "--name" => {
                i += 1;
                name = args[i].clone();
            }
            "--connect" => {
                i += 1;
                connect_to = Some(args[i].clone());
            }
            other => {
                eprintln!("Unknown arg: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let identity = PrivateKey::generate();
    let peer_id = identity.public_key().peer_id();

    eprintln!("[{name}] PeerId: {peer_id}");
    eprintln!("[{name}] Bind: {bind_addr}");
    eprintln!("[{name}] Roles: {}{}{}",
        if relay { "relay " } else { "" },
        if registry { "registry " } else { "" },
        if !relay && !registry { "peer" } else { "" },
    );

    let node = Node::start(NodeConfig {
        identity,
        bind_addr,
        bootstrap,
        relay,
        registry,
    })
    .await?;

    let local_addr = node.local_addr()?;
    eprintln!("[{name}] Listening on {local_addr}");
    eprintln!("[{name}] Ready.");

    if let Some(target) = connect_to {
        let target_pid = ntied_transport::PeerId::parse(&target)
            .ok_or_else(|| format!("Invalid PeerId '{target}'"))?;
        eprintln!("[{name}] Connecting to {target_pid}...");
        match node.connect(&target_pid).await {
            Ok(conn) => {
                eprintln!("[{name}] Connected! Session {}", conn.session_id());
                let stream = conn.open_stream(1).await?;
                stream.send(format!("Hello from {name}!").as_bytes()).await?;
                eprintln!("[{name}] Sent greeting.");

                let (reply_stream, _purpose) = conn.accept_stream().await?;
                let data = reply_stream.recv().await?;
                eprintln!("[{name}] Received: {}", String::from_utf8_lossy(&data));
            }
            Err(e) => {
                eprintln!("[{name}] Connect failed: {e}");
            }
        }
    } else {
        eprintln!("[{name}] Waiting for connections...");
        loop {
            match node.accept().await {
                Ok(conn) => {
                    let peer = conn.peer_id().await;
                    eprintln!("[{name}] Accepted connection from {peer:?}, session {}", conn.session_id());

                    let (stream, purpose) = conn.accept_stream().await?;
                    let data = stream.recv().await?;
                    eprintln!("[{name}] Received (purpose={purpose}): {}", String::from_utf8_lossy(&data));

                    let reply = conn.open_stream(1).await?;
                    reply.send(format!("Hello back from {name}!").as_bytes()).await?;
                    eprintln!("[{name}] Sent reply.");
                }
                Err(e) => {
                    eprintln!("[{name}] Accept error: {e}");
                }
            }
        }
    }

    // Keep alive
    tokio::signal::ctrl_c().await?;
    eprintln!("[{name}] Shutting down.");
    Ok(())
}
