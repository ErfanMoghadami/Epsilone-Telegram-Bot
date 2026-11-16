use std::{
    collections::HashSet,
    error::Error,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use reqwest::Url;

use serde::{Deserialize, Serialize};

use teloxide::{
    payloads::SendMessageSetters,
    prelude::Requester,
    requests::Request,
    types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup},
};

use crate::{data::LinkClaimError, state::AppState};

#[derive(Clone)]
struct NotificationApiState {
    bot: teloxide::Bot,
    app: AppState,
    secret: Arc<String>,
    in_flight: Arc<Mutex<HashSet<String>>>,
}

#[derive(Serialize)]
struct ApiReply {
    ok: bool,
    status: &'static str,
}

#[derive(Deserialize)]
struct ClaimLinkRequest {
    producer_id: String,
    code: String,
}

#[derive(Deserialize)]
struct ProducerIdQuery {
    producer_id: String,
}

#[derive(Deserialize)]
struct ProducerIdRequest {
    producer_id: String,
}

#[derive(Serialize)]
struct ProducerLinkStatus {
    ok: bool,
    linked: bool,
}

#[derive(Deserialize)]
struct NotificationRequest {
    event_id: String,
    producer_id: String,
    event_type: String,
    title: String,
    body: String,
    action_url: Option<String>,
}

pub async fn run(
    bot: teloxide::Bot,
    app: AppState,
    bind: String,
    secret: String,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if secret.len() < 32 {
        return Err("TELEGRAM_INTERNAL_API_SECRET must be at least 32 characters".into());
    }

    let address: SocketAddr = bind.parse()?;

    let api_state = NotificationApiState {
        bot,
        app,
        secret: Arc::new(secret),
        in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let router = Router::new()
        .route("/health", get(health))
        .route(
            "/internal/v1/producer-links/claim",
            post(claim_producer_link),
        )
        .route(
            "/internal/v1/producer-links/status",
            get(producer_link_status),
        )
        .route("/internal/v1/producer-links/unlink", post(unlink_producer))
        .route("/internal/v1/notifications", post(send_notification))
        .with_state(api_state);

    let listener = tokio::net::TcpListener::bind(address).await?;

    log::info!("Producer notification API listening on {}", address);

    axum::serve(listener, router).await?;

    Ok(())
}

fn api_reply(status: StatusCode, ok: bool, result: &'static str) -> Response {
    (status, Json(ApiReply { ok, status: result })).into_response()
}

async fn health() -> Json<ApiReply> {
    Json(ApiReply {
        ok: true,
        status: "running",
    })
}

fn authorized(headers: &HeaderMap, secret: &str) -> bool {
    let Some(header) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    let Some(token) = header.strip_prefix("Bearer ") else {
        return false;
    };

    constant_time_equal(token.as_bytes(), secret.as_bytes())
}

fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.iter()
        .zip(b.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn valid_field(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(|c| c == '\0')
}

async fn claim_producer_link(
    State(api): State<NotificationApiState>,
    headers: HeaderMap,
    Json(payload): Json<ClaimLinkRequest>,
) -> Response {
    if !authorized(&headers, &api.secret) {
        return api_reply(StatusCode::UNAUTHORIZED, false, "unauthorized");
    }

    if !valid_field(&payload.producer_id, 128) || payload.code.len() != 10 {
        return api_reply(StatusCode::BAD_REQUEST, false, "invalid_payload");
    }

    match api
        .app
        .notifications
        .claim_link_code(&payload.producer_id, &payload.code)
    {
        Ok(()) => api_reply(StatusCode::OK, true, "producer_linked"),

        Err(LinkClaimError::InvalidProducerId) => {
            api_reply(StatusCode::BAD_REQUEST, false, "invalid_producer_id")
        }

        Err(LinkClaimError::InvalidOrExpiredCode) => {
            api_reply(StatusCode::NOT_FOUND, false, "invalid_or_expired_code")
        }

        Err(LinkClaimError::ExpiredCode) => api_reply(StatusCode::GONE, false, "expired_code"),

        Err(LinkClaimError::ProducerAlreadyLinked) => {
            api_reply(StatusCode::CONFLICT, false, "producer_already_linked")
        }

        Err(LinkClaimError::ChatAlreadyLinked) => api_reply(
            StatusCode::CONFLICT,
            false,
            "telegram_account_already_linked",
        ),
    }
}

async fn producer_link_status(
    State(api): State<NotificationApiState>,
    headers: HeaderMap,
    Query(query): Query<ProducerIdQuery>,
) -> Response {
    if !authorized(&headers, &api.secret) {
        return api_reply(StatusCode::UNAUTHORIZED, false, "unauthorized");
    }

    if !valid_field(&query.producer_id, 128) {
        return api_reply(StatusCode::BAD_REQUEST, false, "invalid_producer_id");
    }

    let linked = api
        .app
        .notifications
        .chat_id_for_producer(&query.producer_id)
        .is_some();

    (
        StatusCode::OK,
        Json(ProducerLinkStatus { ok: true, linked }),
    )
        .into_response()
}

async fn unlink_producer(
    State(api): State<NotificationApiState>,
    headers: HeaderMap,
    Json(payload): Json<ProducerIdRequest>,
) -> Response {
    if !authorized(&headers, &api.secret) {
        return api_reply(StatusCode::UNAUTHORIZED, false, "unauthorized");
    }

    if !valid_field(&payload.producer_id, 128) {
        return api_reply(StatusCode::BAD_REQUEST, false, "invalid_producer_id");
    }

    let removed = api.app.notifications.unlink_producer(&payload.producer_id);

    if removed {
        api_reply(StatusCode::OK, true, "producer_unlinked")
    } else {
        api_reply(StatusCode::OK, true, "already_unlinked")
    }
}

fn safe_action_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;

    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }

    let host = url.host_str()?;

    let is_epsilone_domain = host == "epsilone.ir" || host.ends_with(".epsilone.ir");

    if !is_epsilone_domain {
        return None;
    }

    Some(url)
}

async fn send_notification(
    State(api): State<NotificationApiState>,
    headers: HeaderMap,
    Json(payload): Json<NotificationRequest>,
) -> Response {
    if !authorized(&headers, &api.secret) {
        return api_reply(StatusCode::UNAUTHORIZED, false, "unauthorized");
    }

    if !valid_field(&payload.event_id, 128)
        || !valid_field(&payload.producer_id, 128)
        || !valid_field(&payload.event_type, 64)
        || !valid_field(&payload.title, 160)
        || !valid_field(&payload.body, 3500)
        || !payload
            .event_type
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return api_reply(StatusCode::BAD_REQUEST, false, "invalid_payload");
    }

    let action_url = match payload.action_url.as_deref() {
        Some(value) => match safe_action_url(value) {
            Some(url) => Some(url),
            None => {
                return api_reply(StatusCode::BAD_REQUEST, false, "invalid_action_url");
            }
        },

        None => None,
    };

    let Some(chat_id) = api
        .app
        .notifications
        .chat_id_for_producer(&payload.producer_id)
    else {
        // Don't mark the event processed. The website may retry
        // after the producer links their Telegram account.
        return api_reply(StatusCode::CONFLICT, false, "producer_not_linked");
    };

    let event_key = format!("{}:{}", payload.producer_id, payload.event_id);

    if api.app.notifications.is_event_processed(&event_key) {
        return api_reply(StatusCode::OK, true, "duplicate_event_ignored");
    }

    {
        let Ok(mut in_flight) = api.in_flight.lock() else {
            return api_reply(StatusCode::INTERNAL_SERVER_ERROR, false, "lock_unavailable");
        };

        if !in_flight.insert(event_key.clone()) {
            return api_reply(StatusCode::ACCEPTED, true, "event_already_processing");
        }
    }

    let text = format!(
        "🔔 {}\n\n{}\n\nEvent: {}",
        payload.title, payload.body, payload.event_type,
    );

    let result = if let Some(url) = action_url {
        let keyboard =
            InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::url("Open Epsilone", url)]]);

        api.bot
            .send_message(ChatId(chat_id), text)
            .reply_markup(keyboard)
            .send()
            .await
    } else {
        api.bot.send_message(ChatId(chat_id), text).send().await
    };

    if let Ok(mut in_flight) = api.in_flight.lock() {
        in_flight.remove(&event_key);
    }

    match result {
        Ok(_) => {
            // Persist only after Telegram confirms the message was sent.
            api.app.notifications.mark_event_processed(&event_key);

            api_reply(StatusCode::OK, true, "notification_sent")
        }

        Err(error) => {
            log::error!(
                "Producer Telegram notification failed. Event {}: {}",
                payload.event_id,
                error
            );

            // Do not mark as processed. The website can retry.
            api_reply(StatusCode::BAD_GATEWAY, false, "telegram_delivery_failed")
        }
    }
}
