//! The agent-mail HTTP API (`/v1`).

pub mod attention;
pub mod auth;
pub mod cluster;
pub mod commands;
mod directory;
pub mod error;
pub mod events;
mod mail;
mod nodes;
pub mod presence;
mod render;
mod send;
pub mod transcript;
mod ui;
pub mod web;
pub mod webhook;

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
    pub presence: Arc<presence::PresenceBoard>,
    /// Commands for nodes (sleep, wake, transcript leases).
    pub nodes: Arc<commands::NodeHub>,
    /// Conversation mirrors being watched.
    pub mirror: Arc<transcript::MirrorHub>,
    pub mirror_config: transcript::MirrorConfig,
    /// Client for notification webhooks.
    pub http: reqwest::Client,
    pub webhook_config: webhook::WebhookConfig,
    /// Set when several instances share the database (Postgres).
    pub cluster: Option<Arc<cluster::Cluster>>,
    pub attention: attention::Config,
}

impl AppState {
    /// A single server instance.
    pub fn new(store: Arc<dyn Store>) -> Self {
        AppState {
            presence: Arc::new(presence::PresenceBoard::new(store.clone())),
            store,
            bus: Arc::new(Hub::new()),
            nodes: Arc::new(commands::NodeHub::new()),
            mirror: Arc::new(transcript::MirrorHub::new()),
            mirror_config: transcript::MirrorConfig::default(),
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("building the HTTP client"),
            webhook_config: webhook::WebhookConfig::default(),
            cluster: None,
            attention: attention::Config::default(),
        }
    }

    /// An instance of possibly several sharing one Postgres database at
    /// `db_url`; `url` is where the other instances can reach this one.
    pub fn clustered(store: Arc<dyn Store>, db_url: &str, url: Option<String>) -> Self {
        let c = cluster::Cluster::new(db_url, url);
        let mut st = AppState::new(store.clone());
        st.presence = Arc::new(presence::PresenceBoard::shared(store));
        st.bus = Arc::new(cluster::ClusterBus {
            hub: Hub::new(),
            cluster: c.clone(),
        });
        st.cluster = Some(c);
        st
    }

    /// Tell other instances that the presence of these seats changed.
    pub(crate) fn presence_changed(&self, org_id: i64, seats: Vec<String>) {
        if let (Some(c), false) = (&self.cluster, seats.is_empty()) {
            c.notify(cluster::Wire::Presence {
                o: org_id,
                s: seats,
            });
        }
    }
}

/// Start the background work of an instance: cluster membership (when
/// clustered), acknowledgement escalation and cleanup.
pub fn start_background(state: &AppState) -> Vec<tokio::task::JoinHandle<()>> {
    let mut tasks = Vec::new();
    if let Some(c) = &state.cluster {
        tasks.extend(c.start(state));
    }
    tasks.push(tokio::spawn(attention::run(state.clone())));
    tasks
}

pub fn router(state: AppState) -> Router {
    use directory as d;
    let v1 = Router::new()
        .route("/messages", post(mail::send))
        .route("/messages/{id}", get(mail::read))
        .route("/messages/{id}/ack", post(mail::ack))
        .route("/messages/{id}/status", get(mail::status))
        .route("/messages/{id}/read", post(ui::mark_read))
        .route("/session", post(web::login).delete(web::logout))
        .route("/overview", get(ui::overview))
        .route("/counts", get(ui::counts))
        .route("/attention", get(ui::attention))
        .route("/sent", get(ui::sent))
        .route("/archive/search", get(ui::search))
        .route(
            "/focus",
            put(attention::put_focus).delete(attention::delete_focus),
        )
        .route("/seats/{name}/sleep", post(ui::sleep))
        .route("/seats/{name}/wake", post(ui::wake))
        .route(
            "/seats/{name}/transcript",
            get(transcript::watch).post(transcript::post),
        )
        .route("/threads/{id}", get(mail::thread))
        .route("/inbox", get(mail::inbox))
        .route("/claim", post(mail::claim))
        .route("/changes", get(mail::changes))
        .route("/events", get(mail::events))
        .route("/archive/messages", get(mail::archive))
        .route("/whoami", get(d::whoami))
        .route("/org", get(d::get_org).patch(d::patch_org))
        .route("/tokens", get(d::list_tokens).post(d::org_token))
        .route("/tokens/{id}", axum::routing::delete(d::revoke_token))
        .route("/pending", get(nodes::pending))
        .route("/presence", post(nodes::presence))
        .route("/seats/{name}/launch", get(nodes::launch))
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
    let v1 = v1
        .fallback(|| async { error::ApiError::not_found("no such endpoint") })
        .method_not_allowed_fallback(|| async {
            error::ApiError::new(
                axum::http::StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "method not allowed",
            )
        });
    Router::new()
        .nest("/v1", v1)
        .route("/healthz", get(|| async { "ok" }))
        .fallback(web::static_file)
        .layer(axum::middleware::from_fn(web::headers))
        .with_state(state)
}

/// Serve the API on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: AppState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let tasks = start_background(&state);
    // Accept requests only once the other instances' notifications arrive
    // here; connections wait in the listener's backlog meanwhile.
    if let Some(c) = &state.cluster
        && !c.wait_listening(std::time::Duration::from_secs(15)).await
    {
        tracing::warn!("cluster: not listening yet; serving anyway");
    }
    let r = axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(shutdown)
        .await;
    for t in tasks {
        t.abort();
    }
    if let Some(c) = &state.cluster {
        c.leave(&*state.store).await;
    }
    r
}
