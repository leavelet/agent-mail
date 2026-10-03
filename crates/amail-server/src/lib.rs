//! The agent-mail HTTP API (`/v1`).

pub mod auth;
mod directory;
pub mod error;
pub mod events;
pub mod limits;
mod mail;
mod render;
mod send;

use amail_store::Store;
use axum::Router;
use axum::routing::{get, post, put};
use std::sync::Arc;

pub use auth::{hash_token, new_token};
pub use events::{EventBus, Hub};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn Store>,
    pub bus: Arc<dyn EventBus>,
    pub limiter: Arc<limits::RateLimiter>,
}

impl AppState {
    pub fn new(store: Arc<dyn Store>) -> Self {
        AppState {
            store,
            bus: Arc::new(Hub::new()),
            limiter: Arc::new(limits::RateLimiter::default()),
        }
    }
}

pub fn router(state: AppState) -> Router {
    use directory as d;
    let v1 = Router::new()
        .route("/messages", post(mail::send))
        .route("/messages/{id}", get(mail::read))
        .route("/messages/{id}/ack", post(mail::ack))
        .route("/messages/{id}/status", get(mail::status))
        .route("/threads/{id}", get(mail::thread))
        .route("/inbox", get(mail::inbox))
        .route("/claim", post(mail::claim))
        .route("/changes", get(mail::changes))
        .route("/events", get(mail::events))
        .route("/archive/messages", get(mail::archive))
        .route("/whoami", get(d::whoami))
        .route("/org", get(d::get_org).patch(d::patch_org))
        .route("/tokens", post(d::org_token))
        .route("/seats", get(d::list_seats).post(d::create_seat))
        .route(
            "/seats/{name}",
            get(d::get_seat).patch(d::patch_seat).delete(d::delete_seat),
        )
        .route("/seats/{name}/tokens", post(d::seat_token))
        .route("/lists", get(d::list_lists).post(d::create_list))
        .route("/lists/{name}", get(d::get_list).delete(d::delete_list))
        .route(
            "/lists/{name}/members/{address}",
            put(d::put_member).delete(d::delete_member),
        )
        .route("/links", get(d::list_links).post(d::request_link))
        .route("/links/{org}/accept", post(d::accept_link))
        .route("/links/{org}", axum::routing::delete(d::revoke_link));
    Router::new()
        .nest("/v1", v1)
        .route("/healthz", get(|| async { "ok" }))
        .fallback(|| async { error::ApiError::not_found("no such endpoint") })
        .method_not_allowed_fallback(|| async {
            error::ApiError::new(
                axum::http::StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "method not allowed",
            )
        })
        .with_state(state)
}

/// Serve the API on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}
