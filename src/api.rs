use axum::{
    routing::{get, post},
    Router, Json, extract::State,
    response::{IntoResponse, Response},
    body::Body,
    http::header,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use crate::network::{InferenceRequest, InferenceResponse};

#[derive(Clone)]
pub struct AppState {
    pub network_tx: mpsc::Sender<NetworkCommand>,
    pub access_key: Option<String>,
    pub local_llm_api_key: Option<String>,
    // pub local_llm: Option<String>,
    // pub hosted_models: Vec<String>,
    // pub provider_config: Option<crate::config::ProviderConfig>,
    // pub request_timestamps: std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
    // /TUTU
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
    },
    OpenInferenceStream {
        peer: String,
        request: InferenceRequest,
        // We will send chunks of bytes back over this channel as they arrive from the P2P stream
        chunk_tx: mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
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
    headers: axum::http::HeaderMap,
    Query(query): Query<ModelQuery>,
) -> Response {
    // 0. Check access key if configured
    if let Some(ref required_key) = state.access_key {
        let auth_header = headers.get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok());
        
        let provided_key = auth_header.and_then(|h| h.strip_prefix("Bearer "));
        
        if provided_key != Some(required_key) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "Invalid or missing access key" }))
            ).into_response();
        }
    }

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
        }).into_response();
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
    }).into_response()
}

async fn chat_completions(
    State(state): State<AppState>, 
    headers: axum::http::HeaderMap,
    Json(mut payload): Json<InferenceRequest>
) -> Response {
    let auth_header = headers.get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());
    
    let provided_key = auth_header.and_then(|h| h.strip_prefix("Bearer "));
    
    // 0. Forward the access key to the P2P payload!
    payload.access_key = provided_key.map(|s| s.to_string());

    // 1. Check access key if configured for THIS local API proxy
    if let Some(ref required_key) = state.access_key {
        if provided_key != Some(required_key.as_str()) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "Invalid or missing access key" }))
            ).into_response();
        }
    }

    let model_name = payload.model.clone();
    let is_streaming = payload.stream.unwrap_or(false);
    
    // // TUTU
    // // 0. Check if we host this model locally!
    // if state.hosted_models.contains(&model_name) {
    //     if let Some(local_url) = state.local_llm {
    //         // Apply config restrictions before routing locally
    //         if let Some(ref config) = state.provider_config {
    //             let context_length: u32 = payload.messages.iter().map(|m| m.content.len() as u32).sum();
                
    //             if let Err(err_msg) = config.validate_request(&state.request_timestamps, context_length, provided_key) {
    //                 if is_streaming {
    //                     let stream = async_stream::stream! { yield Ok::<_, std::io::Error>(bytes::Bytes::from(format!("data: {{\"error\": \"{}\"}}\n\n", err_msg))); };
    //                     return Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from_stream(stream)).unwrap();
    //                 } else { 
    //                     return Json(serde_json::json!({ "error": err_msg })).into_response(); 
    //                 }
    //             }
    //         }

    //         let full_url = format!("{}/chat/completions", local_url.trim_end_matches('/'));
    //         let mut client_builder = reqwest::Client::builder();
    //         if let Some(ref api_key) = state.local_llm_api_key {
    //             let mut headers = header::HeaderMap::new();
    //             headers.insert(header::AUTHORIZATION, header::HeaderValue::from_str(&format!("Bearer {}", api_key)).unwrap());
    //             client_builder = client_builder.default_headers(headers);
    //         }
    //         let client = client_builder.build().unwrap();
            
    //         if is_streaming {
    //             let stream = async_stream::stream! {
    //                 match client.post(&full_url).json(&payload).send().await {
    //                     Ok(mut res) => {
    //                         while let Some(chunk) = res.chunk().await.unwrap_or(None) {
    //                             yield Ok::<_, std::io::Error>(chunk);
    //                         }
    //                     }
    //                     Err(e) => {
    //                         let err_msg = format!("data: {{\"error\": \"Local Backend Error: {}\"}}\n\n", e);
    //                         yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_msg));
    //                     }
    //                 }
    //             };
    //             return Response::builder()
    //                 .header(header::CONTENT_TYPE, "text/event-stream")
    //                 .header(header::CACHE_CONTROL, "no-cache")
    //                 .header(header::CONNECTION, "keep-alive")
    //                 .body(Body::from_stream(stream))
    //                 .unwrap();
    //         } else {
    //             match client.post(&full_url).json(&payload).send().await {
    //                 Ok(res) => {
    //                     let bytes = res.bytes().await.unwrap_or_default();
    //                     return Response::builder()
    //                         .header(header::CONTENT_TYPE, "application/json")
    //                         .body(Body::from(bytes))
    //                         .unwrap();
    //                 }
    //                 Err(e) => {
    //                     return Json(serde_json::json!({ "error": format!("Local Backend Error: {}", e) })).into_response();
    //                 }
    //             }
    //         }
    //     }
    // }
    
    // 1. Find a provider for the model via DHT
    let (tx_providers, rx_providers) = tokio::sync::oneshot::channel();
    let cmd = NetworkCommand::GetProviders {
        model: model_name.clone(),
        responder: tx_providers,
    };
    
    if state.network_tx.send(cmd).await.is_err() {
        return Json(serde_json::json!({ "error": "Network channel closed" })).into_response();
    }

    let providers = match tokio::time::timeout(std::time::Duration::from_secs(5), rx_providers).await {
        Ok(Ok(p)) => p,
        _ => return Json(serde_json::json!({ "error": "DHT query timed out or failed" })).into_response(),
    };

    if providers.is_empty() {
        return Json(serde_json::json!({ "error": format!("No providers found for model {}", model_name) })).into_response();
    }

    // Just pick the first provider for now
    let peer = providers[0].clone();

    if is_streaming {
        // --- STREAMING MODE ---
        let (chunk_tx, mut chunk_rx) = mpsc::channel(128);
        
        // Ask the network loop to open a P2P stream to this peer
        let cmd = NetworkCommand::OpenInferenceStream {
            peer,
            request: payload,
            chunk_tx,
        };

        if state.network_tx.send(cmd).await.is_err() {
            return Json(serde_json::json!({ "error": "Network channel closed" })).into_response();
        }

        // Create an Async Stream of Raw Bytes
        let stream = async_stream::stream! {
            while let Some(chunk_result) = chunk_rx.recv().await {
                match chunk_result {
                    Ok(bytes) => {
                        yield Ok::<_, std::io::Error>(bytes);
                    }
                    Err(e) => {
                        let err_msg = format!("data: {{\"error\": \"Stream error: {}\"}}\n\n", e);
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from(err_msg));
                        break;
                    }
                }
            }
        };

        return Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::from_stream(stream))
            .unwrap();
        
    } else {
        // --- ONE-SHOT MODE ---
        let (tx_infer, rx_infer) = tokio::sync::oneshot::channel();
        let cmd = NetworkCommand::SendInferenceRequest {
            peer,
            request: payload,
            responder: tx_infer,
        };

        if state.network_tx.send(cmd).await.is_err() {
            return Json(serde_json::json!({ "error": "Network channel closed" })).into_response();
        }

        // Wait for the response and return it
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx_infer).await {
            Ok(Ok(Some(response))) => Json(serde_json::to_value(response).unwrap()).into_response(),
            _ => Json(serde_json::json!({ "error": "Request timed out or failed" })).into_response(),
        }
    }
}

async fn completions(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    // 0. Check access key if configured
    if let Some(ref required_key) = state.access_key {
        let auth_header = headers.get(header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok());
        
        let provided_key = auth_header.and_then(|h| h.strip_prefix("Bearer "));
        
        if provided_key != Some(required_key) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({ "error": "Invalid or missing access key" }))
            ).into_response();
        }
    }

    Json(serde_json::json!({
        "error": "/v1/completions is a stub and not implemented yet. Use /v1/chat/completions."
    })).into_response()
}
