mod config;
mod network;
mod api;

use clap::Parser;
use futures::StreamExt;
use libp2p::{Multiaddr, SwarmBuilder};
use libp2p_identify as identify;
use libp2p_identity as identity;
use libp2p_identity::PeerId;
use libp2p_kad as kad;
use libp2p_noise as noise;
use libp2p_request_response::{self as request_response, ProtocolSupport};
use libp2p_swarm::{StreamProtocol, SwarmEvent};
use libp2p_tcp as tcp;
use libp2p_yamux as yamux;
use std::error::Error;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, Level};

#[derive(Parser, Debug)]
#[command(name = "conduit")]
#[command(about = "P2P LLM Network Node", version = "0.1")]
struct Cli {
    /// Port to listen on for P2P connections
    #[arg(short, long, default_value_t = 0)]
    p2p_port: u16,

    /// Multiaddr of a bootstrap node to connect to
    #[arg(short, long)]
    bootstrap: Option<String>,

    /// Models this node is willing to serve
    #[arg(short = 'm', long)]
    models: Vec<String>,
    
    /// Port to listen on for local HTTP proxy
    #[arg(long, default_value_t = 8080)]
    http_port: u16,

    /// URL of local LLM backend to proxy requests to (e.g. http://192.168.0.124:8080/v1)
    #[arg(long)]
    local_llm: Option<String>,

    /// Path to config.yml file with rate limits and scheduling
    #[arg(long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let cli = Cli::parse();

    let provider_config = if let Some(ref path) = cli.config {
        match config::ProviderConfig::load(path) {
            Ok(cfg) => {
                info!("Loaded configuration from {}: {:?}", path, cfg);
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!("Failed to load config from {}: {:?}", path, e);
                None
            }
        }
    } else {
        None
    };

    // Generate a keypair and PeerId for this node
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());

    info!("Node initialized. PeerId: {}", local_peer_id);

    // Initialize Network Behaviour
    let behaviour = |key: &identity::Keypair| {
        let identify = identify::Behaviour::new(identify::Config::new(
            "/conduit/1.0.0".to_string(),
            key.public(),
        ));

        let kademlia = kad::Behaviour::new(
            local_peer_id,
            kad::store::MemoryStore::new(local_peer_id),
        );

        let request_response = request_response::json::Behaviour::new(
            [(StreamProtocol::new("/conduit/inference/1.0.0"), ProtocolSupport::Full)],
            request_response::Config::default(),
        );

        let stream = libp2p_stream::Behaviour::new();

        network::ConduitBehaviour {
            identify,
            kademlia,
            request_response,
            stream,
        }
    };

    // Build the Swarm
    let mut swarm = SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(behaviour)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    // Change kademlia mode to server so we always add ourselves to the DHT
    swarm.behaviour_mut().kademlia.set_mode(Some(kad::Mode::Server));

    // Listen on the specified port, or OS-assigned if 0
    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", cli.p2p_port).parse()?;
    swarm.listen_on(listen_addr)?;

    // Connect to bootstrap node if provided
    if let Some(bootstrap_addr_str) = cli.bootstrap {
        let bootstrap_addr = Multiaddr::from_str(&bootstrap_addr_str)?;
        info!("Dialing bootstrap node: {}", bootstrap_addr);
        swarm.dial(bootstrap_addr)?;
    }

    // Set up channels
    let (network_tx, mut network_rx) = mpsc::channel(32);
    let (response_tx, mut response_rx) = mpsc::channel(32);
    let app_state = api::AppState { network_tx };

    // Spawn the API server
    let http_port = cli.http_port;
    tokio::spawn(async move {
        api::start_api_server(app_state, http_port).await;
    });

    info!("Starting Swarm event loop...");

    // Start providing models immediately (only if we have a local LLM configured)
    let local_llm_url = provider_config.as_ref().and_then(|c| c.local_llm.clone());
    
    if local_llm_url.is_none() && !cli.models.is_empty() {
        tracing::warn!("Models were provided via --models, but no local_llm backend is configured in the config file. This node will NOT announce itself as a provider to the network.");
    } else if local_llm_url.is_some() {
        for model in &cli.models {
            let key = kad::RecordKey::new(&model.as_bytes());
            info!("Registering model as provider: {}", model);
            if let Err(e) = swarm.behaviour_mut().kademlia.start_providing(key) {
                tracing::error!("Failed to start providing model locally: {:?}", e);
            }
        }
    }

    let mut pending_queries = std::collections::HashMap::new();
    let mut pending_requests = std::collections::HashMap::new();
    let mut announce_interval = tokio::time::interval(std::time::Duration::from_secs(30));

    // Rate limiting state
    let mut request_timestamps: Vec<std::time::Instant> = Vec::new();

    // Accept incoming raw P2P streams
    let mut incoming_streams = swarm.behaviour().stream.new_control().accept(StreamProtocol::new("/conduit/stream/1.0.0")).unwrap();

    loop {
        tokio::select! {
            _ = announce_interval.tick() => {
                let local_llm_url = provider_config.as_ref().and_then(|c| c.local_llm.clone());
                if local_llm_url.is_some() {
                    for model in &cli.models {
                        let key = kad::RecordKey::new(&model.as_bytes());
                        // Periodically push to the network
                        let _ = swarm.behaviour_mut().kademlia.start_providing(key);
                    }
                }
            },
            Some((channel, response)) = response_rx.recv() => {
                let _ = swarm.behaviour_mut().request_response.send_response(channel, response);
            },
            Some((peer_id, mut stream)) = incoming_streams.next() => {
                info!("Incoming P2P inference stream requested from peer {}!", peer_id);
                
                let config_opt = provider_config.clone();

                tokio::spawn(async move {
                    use futures::{AsyncReadExt, AsyncWriteExt};

                    let mut buf = vec![0u8; 8192];
                    let size = match tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut buf)).await {
                        Ok(Ok(n)) if n > 0 => n,
                        _ => { tracing::error!("Failed to read inference request from incoming stream"); return; }
                    };

                    let request: network::InferenceRequest = match serde_json::from_slice(&buf[..size]) {
                        Ok(req) => req,
                        Err(e) => { tracing::error!("Failed to parse inference request from stream: {}", e); return; }
                    };

                    info!("Received inference streaming request for model: {}", request.model);
                    
                    if let Some(ref config) = config_opt {
                        if let Some(max_ctx) = config.max_context {
                            let context_length: u32 = request.messages.iter().map(|m| m.content.len() as u32).sum();
                            if context_length > max_ctx {
                                info!("Rejecting stream request: Context length {} exceeds max {}", context_length, max_ctx);
                                let err_msg = format!("data: {{\"error\": \"Provider max context exceeded. Received {}, Max {}\"}}\n\n", context_length, max_ctx);
                                let _ = stream.write_all(err_msg.as_bytes()).await;
                                let _ = stream.flush().await;
                                let _ = stream.close().await;
                                return;
                            }
                        }
                    }
                    
                    let local_llm_url = config_opt.as_ref().and_then(|c| c.local_llm.clone());
                    if let Some(llm_url) = local_llm_url {
                        let full_url = format!("{}/chat/completions", llm_url.trim_end_matches('/'));
                        let client = reqwest::Client::new();
                        
                        match client.post(&full_url).json(&request).send().await {
                            Ok(mut res) => {
                                while let Some(chunk) = res.chunk().await.unwrap_or(None) {
                                    if stream.write_all(&chunk).await.is_err() {
                                        break; // Consumer disconnected
                                    }
                                    let _ = stream.flush().await;
                                }
                            }
                            Err(e) => {
                                tracing::error!("Failed to contact local LLM for streaming: {}", e);
                                let err_msg = format!("data: {{\"error\": \"Backend Error: {}\"}}\n\n", e);
                                let _ = stream.write_all(err_msg.as_bytes()).await;
                                let _ = stream.flush().await;
                            }
                        }
                    } else {
                        let err_msg = "data: {\"error\": \"Provider has no compute backend configured.\"}\n\n";
                        let _ = stream.write_all(err_msg.as_bytes()).await;
                        let _ = stream.flush().await;
                    }
                    
                    let _ = stream.close().await;
                });
            },
            cmd = network_rx.recv() => match cmd {
                Some(api::NetworkCommand::GetProviders { model, responder }) => {
                    info!("API requested providers for model: {}", model);
                    let key = kad::RecordKey::new(&model.as_bytes());
                    let query_id = swarm.behaviour_mut().kademlia.get_providers(key);
                    pending_queries.insert(query_id, responder);
                }
                Some(api::NetworkCommand::SendInferenceRequest { peer, request, responder }) => {
                    info!("Sending inference request to peer: {}", peer);
                    if let Ok(peer_id) = PeerId::from_str(&peer) {
                        let request_id = swarm.behaviour_mut().request_response.send_request(&peer_id, request);
                        pending_requests.insert(request_id, responder);
                    } else {
                        tracing::error!("Invalid peer ID string: {}", peer);
                        let _ = responder.send(None);
                    }
                }
                Some(api::NetworkCommand::OpenInferenceStream { peer, request, chunk_tx }) => {
                    info!("Opening P2P stream to peer: {}", peer);
                    if let Ok(peer_id) = PeerId::from_str(&peer) {
                        let mut control = swarm.behaviour().stream.new_control();
                        
                        tokio::spawn(async move {
                            // Open a stream to the provider node
                            let protocol = StreamProtocol::new("/conduit/stream/1.0.0");
                            match control.open_stream(peer_id, protocol).await {
                                Ok(mut stream) => {
                                    // 1. Send the JSON request
                                    let req_bytes = serde_json::to_vec(&request).unwrap();
                                    use futures::AsyncWriteExt;
                                    if let Err(e) = stream.write_all(&req_bytes).await {
                                        let _ = chunk_tx.send(Err(e)).await;
                                        return;
                                    }
                                    let _ = stream.flush().await;

                                    // 2. Read the SSE chunks as they arrive and pipe them back to api.rs
                                    let mut buf = vec![0u8; 4096];
                                    use futures::AsyncReadExt;
                                    loop {
                                        match stream.read(&mut buf).await {
                                            Ok(0) => break, // EOF
                                            Ok(n) => {
                                                let chunk = bytes::Bytes::copy_from_slice(&buf[..n]);
                                                if chunk_tx.send(Ok(chunk)).await.is_err() {
                                                    break; // HTTP Client disconnected
                                                }
                                            }
                                            Err(e) => {
                                                let _ = chunk_tx.send(Err(e)).await;
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to open P2P stream: {:?}", e);
                                    let _ = chunk_tx.send(Err(std::io::Error::new(std::io::ErrorKind::Other, "Stream failed"))).await;
                                }
                            }
                        });
                    } else {
                        tracing::error!("Invalid peer ID string: {}", peer);
                    }
                }
                None => break,
            },
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    info!("Listening on {:?}", address);
                }
                SwarmEvent::Behaviour(network::ConduitBehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. })) => {
                    info!("Identified peer {}: {:?}", peer_id, info.listen_addrs);
                    // Add addresses to Kademlia routing table
                    for addr in info.listen_addrs {
                        swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
                    }
                }
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                    info!("Connected to {} at {:?}", peer_id, endpoint.get_remote_address());
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    info!("Disconnected from {}", peer_id);
                }
                SwarmEvent::Behaviour(network::ConduitBehaviourEvent::Kademlia(
                    kad::Event::OutboundQueryProgressed { id, result, .. }
                )) => {
                    match result {
                        kad::QueryResult::StartProviding(Ok(kad::AddProviderOk { key })) => {
                            info!("Successfully announced providing model: {:?}", String::from_utf8_lossy(key.as_ref()));
                        }
                        kad::QueryResult::StartProviding(Err(e)) => {
                            tracing::error!("Failed to announce providing model: {:?}", e);
                        }
                        kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders { providers, .. })) => {
                            if let Some(responder) = pending_queries.remove(&id) {
                                let peer_ids: Vec<String> = providers.into_iter().map(|p| p.to_string()).collect();
                                let _ = responder.send(peer_ids);
                            }
                        }
                        kad::QueryResult::GetProviders(Err(e)) => {
                            tracing::error!("Failed to get providers: {:?}", e);
                            if let Some(responder) = pending_queries.remove(&id) {
                                let _ = responder.send(vec![]);
                            }
                        }
                        _ => {}
                    }
                }
                SwarmEvent::Behaviour(network::ConduitBehaviourEvent::RequestResponse(
                    request_response::Event::Message { message, .. }
                )) => {
                    match message {
                        request_response::Message::Request { request, channel, .. } => {
                            info!("Received inference request over P2P for model: {}", request.model);
                            
                            if let Some(ref config) = provider_config {
                                // 1. Schedule Check
                                if !config.is_within_schedule() {
                                    info!("Rejecting request: Outside of configured schedule");
                                    let _ = swarm.behaviour_mut().request_response.send_response(channel, network::InferenceResponse {
                                        id: "error".to_string(),
                                        choices: vec![network::Choice { message: network::ChatMessage { role: "system".to_string(), content: "Provider is currently outside of usage hours.".to_string() } }],
                                    });
                                    continue;
                                }

                                // 2. Rate Limit Check
                                if let Some(ref rate_limit) = config.rate_limit {
                                    let now = std::time::Instant::now();
                                    // Remove timestamps older than 60 seconds
                                    request_timestamps.retain(|t| now.duration_since(*t).as_secs() < 60);

                                    info!("Checking rate limit: {}/{} requests", request_timestamps.len(), rate_limit.requests_per_minute);

                                    if request_timestamps.len() >= rate_limit.requests_per_minute as usize {
                                        info!("Rejecting request: Rate limit exceeded");
                                        let _ = swarm.behaviour_mut().request_response.send_response(channel, network::InferenceResponse {
                                            id: "error".to_string(),
                                            choices: vec![network::Choice { message: network::ChatMessage { role: "system".to_string(), content: "Provider rate limit exceeded.".to_string() } }],
                                        });
                                        continue;
                                    }
                                    
                                    request_timestamps.push(now);
                                } else {
                                    info!("No rate limit configured");
                                }

                                // 3. Max Context Check
                                if let Some(max_ctx) = config.max_context {
                                    let context_length: u32 = request.messages.iter().map(|m| m.content.len() as u32).sum();
                                    if context_length > max_ctx {
                                        info!("Rejecting request: Context length {} exceeds max {}", context_length, max_ctx);
                                        let _ = swarm.behaviour_mut().request_response.send_response(channel, network::InferenceResponse {
                                            id: "error".to_string(),
                                            choices: vec![network::Choice { message: network::ChatMessage { role: "system".to_string(), content: format!("Provider max context exceeded. Received {}, Max {}", context_length, max_ctx) } }],
                                        });
                                        continue;
                                    }
                                }
                            }

                            let local_llm_url = provider_config.as_ref().and_then(|c| c.local_llm.clone());

                            if let Some(llm_url) = local_llm_url {
                                let tx = response_tx.clone();
                                tokio::spawn(async move {
                                    let full_url = format!("{}/chat/completions", llm_url.trim_end_matches('/'));
                                    let client = reqwest::Client::new();
                                    
                                    // Make the request to the local LLM
                                    let res = match client.post(&full_url).json(&request).send().await {
                                        Ok(r) => r,
                                        Err(e) => {
                                            tracing::error!("Failed to contact local LLM: {}", e);
                                            let _ = tx.send((channel, network::InferenceResponse {
                                                id: "error".to_string(),
                                                choices: vec![network::Choice { message: network::ChatMessage { role: "system".to_string(), content: format!("Backend Error: {}", e) } }],
                                            })).await;
                                            return;
                                        }
                                    };
                                    
                                    if let Ok(json) = res.json::<network::InferenceResponse>().await {
                                        let _ = tx.send((channel, json)).await;
                                    } else {
                                        let _ = tx.send((channel, network::InferenceResponse {
                                            id: "error".to_string(),
                                            choices: vec![network::Choice { message: network::ChatMessage { role: "system".to_string(), content: "Invalid response from backend".to_string() } }],
                                        })).await;
                                    }
                                });
                            } else {
                                tracing::error!("Received a request but no local_llm backend URL is configured to serve it!");
                                let _ = swarm.behaviour_mut().request_response.send_response(channel, network::InferenceResponse {
                                    id: "error".to_string(),
                                    choices: vec![network::Choice { message: network::ChatMessage { role: "system".to_string(), content: "Provider has no compute backend configured.".to_string() } }],
                                });
                            }
                        }
                        request_response::Message::Response { request_id, response, .. } => {
                            info!("Received inference response over P2P");
                            if let Some(responder) = pending_requests.remove(&request_id) {
                                let _ = responder.send(Some(response));
                            }
                        }
                    }
                }
                SwarmEvent::Behaviour(network::ConduitBehaviourEvent::RequestResponse(
                    request_response::Event::OutboundFailure { request_id, error, .. }
                )) => {
                    tracing::error!("Failed to send inference request: {:?}", error);
                    if let Some(responder) = pending_requests.remove(&request_id) {
                        let _ = responder.send(None);
                    }
                }
                _ => {}
            }
        }
    }


    Ok(())
}
