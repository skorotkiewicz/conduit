use libp2p_identify as identify;
use libp2p_kad as kad;
use libp2p_request_response as request_response;
use libp2p_swarm::NetworkBehaviour;
use serde::{Deserialize, Serialize};

// --- Data Types ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: Option<bool>,
    pub access_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceResponse {
    pub id: String,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub message: ChatMessage,
}



// --- Network Behaviour ---

#[derive(NetworkBehaviour)]
pub struct ConduitBehaviour {
    pub identify: identify::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub request_response: request_response::json::Behaviour<InferenceRequest, InferenceResponse>,
    pub stream: libp2p_stream::Behaviour,
}
