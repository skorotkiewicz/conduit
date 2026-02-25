use axum::{
    routing::{get, post},
    Router, Json, extract::State,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use crate::network::{InferenceRequest, InferenceResponse};

#[derive(Clone)]
pub struct AppState {
    pub network_tx: mpsc::Sender<NetworkCommand>,
}

#[derive(Debug)]
pub enum NetworkCommand {
    GetProviders {
        model: String,
        responder: tokio::sync::oneshot::Sender<Vec<String>>,
    },
    SendInferenceRequest {
        peer: String,
        request: InferenceRequest,
        responder: tokio::sync::oneshot::Sender<Option<InferenceResponse>>,
    }
}

#[derive(Serialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<ModelData>,
}

#[derive(Serialize)]
pub struct ModelData {
    pub id: String,
    pub object: String,
    pub ready: bool,
}

pub async fn start_api_server(state: AppState, port: u16) {
    let app = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("Starting API proxy on http://{}", addr);
    axum::serve(listener, app).await.unwrap();
}

use axum::extract::Query;

#[derive(Deserialize)]
pub struct ModelQuery {
    pub model: Option<String>,
}

async fn list_models(
    State(state): State<AppState>,
    Query(query): Query<ModelQuery>,
) -> Json<ModelsResponse> {
    let model_name = query.model.unwrap_or_else(|| "default-model".to_string());
    
    let (tx, rx) = tokio::sync::oneshot::channel();
    
    // Send command to the swarm event loop
    let cmd = NetworkCommand::GetProviders {
        model: model_name.clone(),
        responder: tx,
    };
    
    if state.network_tx.send(cmd).await.is_err() {
        return Json(ModelsResponse {
            object: "list".to_string(),
            data: vec![], // Return empty if networking is down
        });
    }

    // Wait for the DHT response with a timeout
    let providers = match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(p)) => p,
        _ => vec![],
    };

    let mut data = Vec::new();
    for provider in providers {
        data.push(ModelData {
            id: format!("{}-via-{}", model_name, provider),
            object: "model".to_string(),
            ready: true,
        });
    }

    Json(ModelsResponse {
        object: "list".to_string(),
        data,
    })
}

async fn chat_completions(State(state): State<AppState>, Json(payload): Json<InferenceRequest>) -> Json<serde_json::Value> {
    let model_name = payload.model.clone();
    
    // 1. Find a provider for the model via DHT
    let (tx_providers, rx_providers) = tokio::sync::oneshot::channel();
    let cmd = NetworkCommand::GetProviders {
        model: model_name.clone(),
        responder: tx_providers,
    };
    
    if state.network_tx.send(cmd).await.is_err() {
        return Json(serde_json::json!({ "error": "Network channel closed" }));
    }

    let providers = match tokio::time::timeout(std::time::Duration::from_secs(5), rx_providers).await {
        Ok(Ok(p)) => p,
        _ => return Json(serde_json::json!({ "error": "DHT query timed out or failed" })),
    };

    if providers.is_empty() {
        return Json(serde_json::json!({ "error": format!("No providers found for model {}", model_name) }));
    }

    // Just pick the first provider for now
    let peer = providers[0].clone();

    // 2. Send the inference request to the provider
    let (tx_infer, rx_infer) = tokio::sync::oneshot::channel();
    let cmd = NetworkCommand::SendInferenceRequest {
        peer,
        request: payload,
        responder: tx_infer,
    };

    if state.network_tx.send(cmd).await.is_err() {
        return Json(serde_json::json!({ "error": "Network channel closed" }));
    }

    // 3. Wait for the response and return it
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx_infer).await {
        Ok(Ok(Some(response))) => Json(serde_json::to_value(response).unwrap()),
        _ => Json(serde_json::json!({ "error": "Request timed out or failed" })),
    }
}

async fn completions() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "error": "/v1/completions is a stub and not implemented yet. Use /v1/chat/completions."
    }))
}
