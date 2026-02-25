# Conduit

A decentralized peer-to-peer network for sharing and accessing Large Language Models (LLMs) through a standardized OpenAI-compatible API. 

Conduit allows users to securely serve their own local models and access models from others globally, creating a distributed and resilient compute network.

## Features

- **OpenAI Compatible:** Acts as a drop-in replacement for any client that uses the standard OpenAI API (`http://localhost:8888/v1`).
- **Decentralized Discovery:** Built on top of `rust-libp2p` and Kademlia DHT for robust peer and model discovery.
- **Provider Safety:** Protect your hardware by configuring precise rate limits and time-based availability schedules.
- **Dynamic Routing:** Automatically discovers and routes requests to the nearest peers serving the exact model you need.

## Installation

Ensure you have [Rust](https://rustup.rs/) installed, then clone and build the project:

```bash
cargo build --release
```

## Quick Start

Conduit nodes can act simultaneously as providers (sharing computation) and consumers (querying the network). When connecting across the internet (WAN), you must use the public IP address or DNS name of a bootstrap node instead of `127.0.0.1`.

### 1. Start a Bootstrap/Provider Node (e.g., At Home)
To share a local LLM and act as an entry point for the network, provide your models and a configuration file. Ensure port `8000` is forwarded on your router if you want external nodes to connect.

```bash
cargo run --release -- \
  --p2p-port 8000 \
  --models "llama-3" \
  --config config.yml
```

### 2. Start a Consumer Node (e.g., In Another Country)
Run a node to connect to the network and query models. You need the public address of your home bootstrap node to join the swarm.

```bash
cargo run --release -- \
  --p2p-port 8001 \
  --http-port 8889 \
  --bootstrap /ip4/YOUR_HOME_PUBLIC_IP/tcp/8000 \
  --access-key "tutu"
```
*Your local proxy API on this travel machine is now listening at `http://localhost:8889/v1` and requires the `Bearer tutu` authorization token.*

#### Provider Configuration (`config.yml`)
Protect your local compute resources by defining rate limits and usage schedules:

```yaml
# Network Settings
http_port: 8888
p2p_port: 8000
bootstrap_nodes: ["/ip4/YOUR_VPS_IP/tcp/8000"] # for now just use the first bootstrap node in the array

# Provider Capabilities
local_llm: "http://127.0.0.1:11434/v1"
local_llm_api_key: null # Optional downstream auth
access_key: "tutu" # Required for P2P/Local clients to access this node
models: ["llama-3", "mistral-7b"]

# Protection Rules
max_context: 5000
rate_limit:
  requests_per_minute: 10
schedule:
  start: "00:00"
  end: "07:00"
```

> [!NOTE] 
> Configuration constraints (schedules, rate limits, max context window) are universally enforced at the routing layer—meaning they apply symmetrically to P2P network queries, long-running P2P streams, and queries submitted directly to your node's local HTTP proxy.

### 3. Dedicated Bootstrap Node (e.g., on a VPS)
If you want to run a node *purely* to help other peers discover each other (without hosting any models or making any queries yourself), you can run Conduit on a cloud VPS. Since it defaults to Kademlia Server mode, it will perfectly act as a backbone router!

```bash
cargo run --release -- \
  --p2p-port 8000 \
  --http-port 9999
```
*Note: This node will participate in the DHT and route traffic for others, but because no `--models` were provided, it won't announce itself as an AI provider.*

Then, everyone else (consumers and providers) can connect using your VPS IP:
`--bootstrap /ip4/YOUR_VPS_IP/tcp/8000`

### 4. Making Requests
Once connected to the swarm, interact with your local Conduit consumer node exactly as you would with the official OpenAI API. Don't forget your configured access key!

```bash
curl -X POST http://localhost:8888/v1/chat/completions \
  -H "Authorization: Bearer tutu" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "llama-3",
    "messages": [
      {"role": "user", "content": "What is the capital of France?"}
    ]
  }'
```

---

*Built with Rust 🦀 and libp2p.*
