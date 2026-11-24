// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
use crate::envelope::Attachment;
use crate::node::AppNode;
use crate::storage::AppStorage;
use axum::extract::{Path, Request, State};
use axum::http::header::{HeaderName, AUTHORIZATION, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::http::{HeaderValue, Method};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use mycena_l0_facade::data::now_ms;
use mycena_l0_facade::energy::NodeState;
use mycena_l0_facade::wipe_node_directory;
use mycena_l1_facade::NodeCommand;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Clone)]
struct ApiState {
    app: Arc<AppNode>,
    storage: Arc<AppStorage>,
    db_path: PathBuf,
    token: String,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    peer_id: String,
    display_name: String,
    peers: Vec<String>,
    metrics: mycena_l1_facade::NodeMetrics,
}

#[derive(Debug, Deserialize)]
struct ChatBody {
    body: String,
}

#[derive(Debug, Deserialize)]
struct BulletinBody {
    title: String,
    body: String,
    ttl_secs: u64,
}

#[derive(Debug, Deserialize)]
struct MailBody {
    subject: String,
    body: String,
    attachments: Option<Vec<Attachment>>,
}

#[derive(Debug, Deserialize)]
struct NameBody {
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct EnergyBody {
    state: String,
}

#[derive(Debug, Deserialize)]
struct AddPeerBody {
    multiaddr: String,
}

#[derive(Debug, Deserialize)]
struct ScopeBody {
    scope: String,
}

#[derive(Debug, Serialize)]
struct StoreStatsResponse {
    count: usize,
    oldest_ms: u64,
    size_estimate_kb: u64,
}

/// Starts the loopback HTTP API. Returns the bearer token (from `MYCENA_API_TOKEN` or random).
pub async fn start_api_server(
    app_node: Arc<AppNode>,
    app_storage: Arc<AppStorage>,
    port: u16,
    db_path: PathBuf,
) -> anyhow::Result<String> {
    let token = load_or_generate_api_token();
    let state = ApiState {
        app: app_node,
        storage: app_storage,
        db_path,
        token: token.clone(),
    };
    let app = Router::new()
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/peers", get(get_peers))
        .route("/api/v1/chat/:peer_id", get(get_chat).post(post_chat))
        .route("/api/v1/chat/broadcast", post(post_chat_broadcast))
        .route(
            "/api/v1/bulletin/:scope",
            get(get_bulletin).post(post_bulletin),
        )
        .route("/api/v1/mail/inbox", get(get_mail_inbox))
        .route("/api/v1/mail/sent", get(get_mail_sent))
        .route("/api/v1/mail/:peer_id", post(post_mail))
        .route("/api/v1/mail/:id/read", put(put_mail_read))
        .route("/api/v1/settings/name", put(put_name))
        .route("/api/v1/settings", get(get_settings))
        .route("/api/v1/settings/energy", put(put_energy))
        .route("/api/v1/store/stats", get(get_store_stats))
        .route("/api/v1/store/gc", post(post_store_gc))
        .route(
            "/api/v1/peers/:peer_id/reputation",
            get(get_peer_reputation),
        )
        .route("/api/v1/peers/add", post(post_peer_add))
        .route("/api/v1/scopes", get(get_scopes))
        .route("/api/v1/scopes/subscribe", post(post_scope_subscribe))
        .route("/api/v1/scopes/:scope", delete(delete_scope))
        .route("/api/v1/groups", get(get_groups).post(post_group))
        .route("/api/v1/groups/join", post(post_group_join))
        .route("/api/v1/groups/:group_id/chat", post(post_group_chat))
        .route("/api/v1/wallet/address", get(get_wallet_address))
        .route("/api/v1/wallet/balance", get(get_wallet_balance))
        .route("/api/v1/wallet/send", post(post_wallet_send))
        .route(
            "/api/v1/volunteer/tasks",
            get(get_volunteer_tasks).post(post_volunteer_task),
        )
        .route(
            "/api/v1/volunteer/tasks/:task_id/claim",
            post(post_volunteer_claim),
        )
        .route("/api/v1/panic/wipe", post(post_panic_wipe))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_token,
        ))
        .route("/api/v1/health", get(api_health))
        .layer(loopback_cors())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("api server exited: {e}");
        }
    });
    Ok(token)
}

fn load_or_generate_api_token() -> String {
    match std::env::var("MYCENA_API_TOKEN") {
        Ok(t) if t.len() >= 16 => t,
        _ => {
            let mut bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut bytes);
            hex::encode(bytes)
        }
    }
}

fn token_matches(provided: &str, expected: &str) -> bool {
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn extract_bearer(req: &Request) -> String {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .map(str::to_string)
        .or_else(|| {
            req.headers()
                .get("x-mycena-token")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

async fn require_api_token(
    State(state): State<ApiState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided = extract_bearer(&req);
    if !token_matches(&provided, &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

async fn api_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

fn loopback_origin(origin: &HeaderValue) -> bool {
    origin.to_str().ok().is_some_and(|o| {
        o.starts_with("http://127.0.0.1")
            || o.starts_with("http://localhost")
            || o.starts_with("http://[::1]")
    })
}

fn loopback_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| loopback_origin(origin)))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            AUTHORIZATION,
            CONTENT_TYPE,
            HeaderName::from_static("x-mycena-token"),
        ])
}

async fn get_status(State(state): State<ApiState>) -> Json<StatusResponse> {
    let metrics = state.app.node_handle().metrics().await;
    Json(StatusResponse {
        peer_id: state.app.local_peer_id().to_string(),
        display_name: state.app.display_name().await,
        peers: Vec::new(),
        metrics,
    })
}

async fn get_peers() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn get_chat(
    Path(peer_id): Path<String>,
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::envelope::ChatMessage>>, (StatusCode, String)> {
    state
        .storage
        .chat_history(&peer_id, 200)
        .map(Json)
        .map_err(internal_err)
}

async fn post_chat(
    Path(peer_id): Path<String>,
    State(state): State<ApiState>,
    Json(body): Json<ChatBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .send_chat(Some(peer_id), body.body)
        .await
        .map(|_| StatusCode::OK)
        .map_err(internal_err)
}

async fn post_chat_broadcast(
    State(state): State<ApiState>,
    Json(body): Json<ChatBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .send_chat(None, body.body)
        .await
        .map(|_| StatusCode::OK)
        .map_err(internal_err)
}

async fn get_bulletin(
    Path(scope): Path<String>,
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::envelope::BulletinPost>>, (StatusCode, String)> {
    state
        .storage
        .bulletins_for_scope(&scope)
        .map(Json)
        .map_err(internal_err)
}

async fn post_bulletin(
    Path(scope): Path<String>,
    State(state): State<ApiState>,
    Json(body): Json<BulletinBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .post_bulletin(scope, body.title, body.body, body.ttl_secs, None)
        .await
        .map(|_| StatusCode::OK)
        .map_err(internal_err)
}

async fn get_mail_inbox(
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::envelope::MailMessage>>, (StatusCode, String)> {
    state.storage.inbox(200).map(Json).map_err(internal_err)
}

async fn get_mail_sent(
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::envelope::MailMessage>>, (StatusCode, String)> {
    state.storage.sent(200).map(Json).map_err(internal_err)
}

async fn post_mail(
    Path(peer_id): Path<String>,
    State(state): State<ApiState>,
    Json(body): Json<MailBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .send_mail(
            peer_id,
            body.subject,
            body.body,
            body.attachments.unwrap_or_default(),
        )
        .await
        .map(|_| StatusCode::OK)
        .map_err(internal_err)
}

async fn put_mail_read(
    Path(id): Path<String>,
    State(state): State<ApiState>,
) -> Result<StatusCode, (StatusCode, String)> {
    let uuid = uuid::Uuid::parse_str(&id).map_err(internal_err)?;
    state
        .storage
        .mark_read(&uuid)
        .map(|_| StatusCode::OK)
        .map_err(internal_err)
}

async fn put_name(
    State(state): State<ApiState>,
    Json(body): Json<NameBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state.app.set_display_name(body.display_name).await;
    Ok(StatusCode::OK)
}

async fn get_settings(State(state): State<ApiState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "display_name": state.app.display_name().await,
        "energy_state": "active"
    }))
}

async fn put_energy(
    State(state): State<ApiState>,
    Json(body): Json<EnergyBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let cmd = match body.state.as_str() {
        "active" => NodeCommand::SetEnergyState(NodeState::Active),
        "intermittent" => NodeCommand::SetEnergyState(NodeState::Intermittent),
        "passive" => NodeCommand::SetEnergyState(NodeState::Passive),
        _ => return Err((StatusCode::BAD_REQUEST, "invalid energy state".to_string())),
    };
    state
        .app
        .node_handle()
        .send(cmd)
        .await
        .map_err(internal_err)?;
    Ok(StatusCode::OK)
}

fn internal_err<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

async fn get_store_stats(
    State(state): State<ApiState>,
) -> Result<Json<StoreStatsResponse>, (StatusCode, String)> {
    let inbox = state.storage.inbox(10_000).map_err(internal_err)?;
    let sent = state.storage.sent(10_000).map_err(internal_err)?;
    let count = inbox.len() + sent.len();
    let oldest_ms = inbox
        .iter()
        .chain(sent.iter())
        .map(|m| m.timestamp_ms)
        .min()
        .unwrap_or(0);
    Ok(Json(StoreStatsResponse {
        count,
        oldest_ms,
        size_estimate_kb: ((count as u64) * 2).max(1),
    }))
}

async fn post_store_gc(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let deleted = state
        .storage
        .prune_expired_bulletins()
        .map_err(internal_err)?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

async fn get_peer_reputation(
    Path(peer_id): Path<String>,
    State(state): State<ApiState>,
) -> Json<serde_json::Value> {
    let rep = state.app.node_handle().peer_reputation(&peer_id).await;
    match rep {
        Some(rep) => Json(serde_json::json!({
            "peer_id": peer_id,
            "strikes": rep.strikes,
            "throttled": now_ms() < rep.throttled_until_ms,
            "throttled_until_ms": rep.throttled_until_ms,
            "reliability": rep.reliability,
            "spam_score": rep.spam_score,
            "relay_success_rate": rep.relay_success_rate,
            "age_ms": rep.age_ms,
            "mutual_contacts": rep.mutual_contacts,
        })),
        None => Json(serde_json::json!({
            "peer_id": peer_id,
            "strikes": 0,
            "throttled": false,
            "throttled_until_ms": 0,
            "reliability": 0.25,
            "spam_score": 0.0,
            "relay_success_rate": 0.25,
            "age_ms": 0,
            "mutual_contacts": 0,
        })),
    }
}

async fn post_peer_add(
    State(state): State<ApiState>,
    Json(body): Json<AddPeerBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .node_handle()
        .send(NodeCommand::AddBootstrapPeer {
            multiaddr: body.multiaddr,
        })
        .await
        .map_err(internal_err)?;
    Ok(StatusCode::OK)
}

async fn get_scopes(State(state): State<ApiState>) -> Json<Vec<String>> {
    Json(state.app.node_handle().subscribed_scopes().await)
}

async fn post_scope_subscribe(
    State(state): State<ApiState>,
    Json(body): Json<ScopeBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .node_handle()
        .send(NodeCommand::SubscribeScope(body.scope))
        .await
        .map_err(internal_err)?;
    Ok(StatusCode::OK)
}

async fn delete_scope(
    Path(scope): Path<String>,
    State(state): State<ApiState>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .node_handle()
        .send(NodeCommand::UnsubscribeScope(scope))
        .await
        .map_err(internal_err)?;
    Ok(StatusCode::OK)
}

#[derive(Debug, Deserialize)]
struct GroupCreateBody {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GroupJoinBody {
    invite_json: String,
}

#[derive(Debug, Deserialize)]
struct GroupChatBody {
    body: String,
}

#[derive(Debug, Deserialize)]
struct VolunteerTaskBody {
    geohash: String,
    category: String,
    description: String,
    slots_needed: u32,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WalletSendBody {
    to: String,
    amount_muon: u64,
}

#[derive(Debug, Deserialize)]
struct PanicWipeBody {
    confirm: bool,
}

async fn post_panic_wipe(
    State(state): State<ApiState>,
    Json(body): Json<PanicWipeBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    if !body.confirm {
        return Err((StatusCode::BAD_REQUEST, "confirm=true required".into()));
    }
    wipe_node_directory(&state.db_path).map_err(internal_err)?;
    Ok(StatusCode::OK)
}

async fn get_groups(
    State(state): State<ApiState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let groups = state.app.list_groups().map_err(internal_err)?;
    Ok(Json(
        groups
            .into_iter()
            .map(|g| {
                serde_json::json!({
                    "id": g.id,
                    "name": g.name,
                    "members": g.members,
                })
            })
            .collect(),
    ))
}

async fn post_group(
    State(state): State<ApiState>,
    Json(body): Json<GroupCreateBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let group = state
        .app
        .create_group(body.name)
        .await
        .map_err(internal_err)?;
    Ok(Json(serde_json::json!({
        "id": group.id,
        "name": group.name,
        "invite": group.export_invite(),
    })))
}

async fn post_group_join(
    State(state): State<ApiState>,
    Json(body): Json<GroupJoinBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let group = state
        .app
        .join_group_from_invite(&body.invite_json)
        .await
        .map_err(internal_err)?;
    Ok(Json(serde_json::json!({
        "id": group.id,
        "name": group.name,
    })))
}

async fn post_group_chat(
    Path(group_id): Path<String>,
    State(state): State<ApiState>,
    Json(body): Json<GroupChatBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .app
        .send_group_message_by_id(&group_id, body.body)
        .await
        .map_err(internal_err)?;
    Ok(StatusCode::OK)
}

async fn get_wallet_address(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let coin = state
        .app
        .coin_node()
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, "coin disabled".into()))?;
    Ok(Json(serde_json::json!({
        "address": coin.local_address(),
        "experimental": true,
        "legal_tender": false,
    })))
}

async fn get_wallet_balance(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let coin = state
        .app
        .coin_node()
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, "coin disabled".into()))?;
    let (confirmed, pending) = coin.balance().map_err(internal_err)?;
    Ok(Json(serde_json::json!({
        "confirmed_muon": confirmed,
        "pending_muon": pending,
        "experimental": true,
        "legal_tender": false,
    })))
}

async fn post_wallet_send(
    State(state): State<ApiState>,
    Json(body): Json<WalletSendBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let coin = state
        .app
        .coin_node()
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, "coin disabled".into()))?;
    coin.submit_transfer_from_identity_path(
        coin.coin_identity_path(),
        body.to,
        body.amount_muon,
        0,
        None,
    )
    .await
    .map_err(internal_err)?;
    Ok(StatusCode::OK)
}

async fn get_volunteer_tasks(
    State(state): State<ApiState>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let tasks = state.app.list_volunteer_tasks().map_err(internal_err)?;
    Ok(Json(
        tasks
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "task_id": t.task_id,
                    "geohash": t.geohash,
                    "category": t.category.as_str(),
                    "description": t.description,
                    "slots_needed": t.slots_needed,
                    "slots_open": t.slots_open(),
                    "claims_count": t.claims.len(),
                })
            })
            .collect(),
    ))
}

async fn post_volunteer_task(
    State(state): State<ApiState>,
    Json(body): Json<VolunteerTaskBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let task = state
        .app
        .post_volunteer_task(
            &body.geohash,
            &body.category,
            &body.description,
            body.slots_needed,
            body.ttl_secs.unwrap_or(0),
        )
        .await
        .map_err(internal_err)?;
    Ok(Json(serde_json::json!({
        "task_id": task.task_id,
        "slots_open": task.slots_open(),
    })))
}

async fn post_volunteer_claim(
    Path(task_id): Path<String>,
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let task = state
        .app
        .claim_volunteer_task(&task_id)
        .await
        .map_err(internal_err)?;
    Ok(Json(serde_json::json!({
        "task_id": task.task_id,
        "slots_open": task.slots_open(),
    })))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn token_eq_rejects_mismatch_and_length() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "ab"));
        assert!(!token_matches("", "secret"));
    }

    #[test]
    fn generated_token_is_long_enough() {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let t = hex::encode(bytes);
        assert_eq!(t.len(), 64);
        assert!(t.len() >= 16);
    }
}
