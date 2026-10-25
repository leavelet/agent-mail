//! The agent-mail HTTP API (`/v1`).

pub mod activity;
pub mod attention;
pub mod auth;
mod bodylimit;
mod boundary;
pub mod cluster;
pub mod commands;
pub mod config;
mod deletion;
mod directory;
pub mod dirwatch;
pub mod error;
pub mod events;
pub mod limits;
mod mail;
pub mod netguard;
mod nodes;
mod notices;
mod openings;
mod operator;
pub mod peer;
pub mod presence;
pub mod public;
mod render;
mod revoke;
mod send;
pub mod tasks;
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
pub use public::new_invite;

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
    /// Public mode, limits, proxies, retention (see [`config`]).
    pub config: Arc<config::Config>,
    pub limiters: Arc<limits::Limiters>,
    /// Open streams by credential, to end them on revocation.
    pub revocations: Arc<revoke::Revocations>,
    /// Recent mail per org, for the overview.
    pub activity: Arc<activity::Activity>,
    /// Seat and list changes, for `directory` events.
    pub directory: Arc<dirwatch::DirectoryHub>,
    /// Task changes, for `task` events.
    pub tasks: Arc<tasks::TaskHub>,
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
            http: netguard::webhook_client(true, None),
            webhook_config: webhook::WebhookConfig::default(),
            cluster: None,
            attention: attention::Config::default(),
            config: Arc::new(config::Config::default()),
            limiters: Arc::new(limits::Limiters::new(&config::Limits::default())),
            revocations: Arc::new(revoke::Revocations::default()),
            activity: Arc::new(activity::Activity::default()),
            directory: Arc::new(dirwatch::DirectoryHub::default()),
            tasks: Arc::new(tasks::TaskHub::default()),
        }
    }

    /// Apply server settings (public mode, limits, proxies, webhook
    /// targets, retention); call before serving.
    pub fn set_config(&mut self, cfg: config::Config) {
        self.http = netguard::webhook_client(cfg.webhook_private_targets, None);
        self.limiters = Arc::new(limits::Limiters::new(&cfg.limits));
        self.mirror = Arc::new(transcript::MirrorHub::with_limit(cfg.limits.mirror_bytes));
        self.config = Arc::new(cfg);
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

    /// A token was revoked: end its streams here and on the other
    /// instances. (Every instance also checks the credentials of its open
    /// streams at each sweep.)
    pub(crate) fn token_revoked(&self, _org_id: i64, token_id: i64) {
        self.credentials_ended(vec![token_id], Vec::new());
    }

    /// End the streams of these tokens and browser sessions (by hash) here
    /// and on the other instances.
    pub(crate) fn credentials_ended(&self, tokens: Vec<i64>, sessions: Vec<String>) {
        self.revocations.end(
            tokens
                .iter()
                .copied()
                .map(revoke::Cred::Token)
                .chain(sessions.iter().cloned().map(revoke::Cred::Session)),
        );
        if let Some(c) = &self.cluster {
            c.notify(cluster::Wire::Ended {
                k: tokens,
                s: sessions,
            });
        }
    }

    /// A seat or list changed: tell this instance's event streams and the
    /// other instances.
    pub(crate) fn directory_changed(&self, org_id: i64, kind: &str, name: &str, change: &str) {
        let ev = dirwatch::event(kind, name, change);
        self.directory.deliver(org_id, ev.clone());
        if let Some(c) = &self.cluster {
            c.notify(cluster::Wire::Directory { o: org_id, d: ev });
        }
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
    tasks.push(tokio::spawn(crate::tasks::run(state.clone())));
    tasks.push(tokio::spawn(activity::run(
        state.activity.clone(),
        state.store.clone(),
    )));
    tasks.push(tokio::spawn(public::sweep(state.clone())));
    if state.config.retention_days.is_some() {
        tasks.push(tokio::spawn(public::retention(state.clone())));
    }
    tasks
}

/// Largest request body (the default of the HTTP framework, made explicit):
/// a message body is at most 256 KB, a node's transcript batch is cut well
/// below this.
pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    use directory as d;
    let v1 = Router::new()
        .route("/info", get(public::info))
        .route("/register", post(public::register))
        .route("/tokens/rotate", post(public::rotate_token))
        .route("/operator/stats", get(operator::stats))
        .route("/operator/orgs/{name}/suspend", post(operator::suspend))
        .route("/operator/orgs/{name}/resume", post(operator::resume))
        .route(
            "/operator/orgs/{name}",
            axum::routing::delete(operator::delete_org),
        )
        .route("/messages", post(mail::send))
        .route("/seats/{name}/messages", post(mail::send_as))
        .route("/messages/preview", post(mail::preview))
        .route("/messages/{id}", get(mail::read))
        .route("/messages/{id}/ack", post(mail::ack))
        .route(
            "/messages/{id}/deliveries/{seat}/withdraw",
            post(mail::withdraw),
        )
        // Compatibility for existing local bridge clients; use upstream withdrawal semantics.
        .route(
            "/messages/{id}/deliveries/{seat}/cancel",
            post(mail::withdraw),
        )
        .route("/messages/{id}/status", get(mail::status))
        .route("/messages/{id}/read", post(ui::mark_read))
        .route("/messages/{id}/unread", post(ui::mark_unread))
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
        .route(
            "/threads/{id}",
            get(mail::thread).delete(deletion::delete_thread),
        )
        .route("/tasks", get(tasks::list).post(tasks::create))
        .route("/tasks/mine", get(tasks::mine))
        .route("/tasks/next", get(tasks::next))
        .route("/tasks/waiting", get(tasks::waiting))
        .route("/tasks/owed", get(tasks::owed))
        .route("/tasks/board", get(tasks::board))
        .route("/tasks/{num}", get(tasks::get).patch(tasks::patch))
        .route("/tasks/{num}/take", post(tasks::take))
        .route("/tasks/{num}/deps", post(tasks::add_dep))
        .route(
            "/tasks/{num}/deps/{other}",
            axum::routing::delete(tasks::remove_dep),
        )
        .route("/tasks/{num}/links", post(tasks::add_link))
        .route(
            "/tasks/{num}/links/{thread}",
            axum::routing::delete(tasks::remove_link),
        )
        .route("/tasks/{num}/comments", post(tasks::comment))
        .route("/tasks/{num}/events", get(tasks::events))
        .route("/inbox", get(mail::inbox))
        .route("/claim", post(mail::claim))
        .route("/claim/release", post(mail::release))
        .route("/changes", get(mail::changes))
        .route("/events", get(mail::events))
        .route("/archive/messages", get(mail::archive))
        .route(
            "/archive/purge-deleted-orgs",
            post(deletion::purge_deleted_orgs),
        )
        .route("/whoami", get(public::whoami))
        .route(
            "/org",
            get(d::get_org)
                .patch(d::patch_org)
                .delete(public::delete_own_org),
        )
        .route("/tokens", get(d::list_tokens).post(d::org_token))
        .route("/tokens/{id}", axum::routing::delete(d::revoke_token))
        .route("/pending", get(nodes::pending))
        .route("/presence", post(nodes::presence))
        .route("/node/notices", post(notices::post))
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
        .route("/org/notice", axum::routing::delete(openings::clear_notice))
        .route("/orgs/{name}", get(openings::peer_org))
        .route("/teams", get(openings::list_teams))
        .route(
            "/teams/{name}",
            get(openings::get_team)
                .put(openings::put_team)
                .delete(openings::delete_team),
        )
        .route(
            "/grants",
            get(openings::list_grants).post(openings::add_grant),
        )
        .route("/grants/preview", post(openings::preview))
        .route(
            "/grants/{id}",
            axum::routing::delete(openings::delete_grant),
        )
        .route("/links", get(d::list_links).post(d::request_link))
        .route("/links/{org}/accept", post(d::accept_link))
        .route(
            "/links/{org}",
            axum::routing::delete(d::revoke_link).patch(d::patch_link),
        );
    let v1 = v1
        .fallback(|| async { error::ApiError::not_found("no such endpoint") })
        .method_not_allowed_fallback(|| async {
            error::ApiError::new(
                axum::http::StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "method not allowed",
            )
        });
    let base = state.config.base_path.clone();
    let app = Router::new()
        .nest("/v1", v1)
        .route("/healthz", get(|| async { "ok" }))
        .fallback(web::static_file)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(axum::middleware::from_fn(bodylimit::layer))
        .layer(axum::middleware::from_fn(web::headers))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            peer::layer,
        ))
        .with_state(state);
    if base.is_empty() {
        return app;
    }
    // Under a path prefix (`--base-path /amail`): everything below it, and
    // `/healthz` at the root too, for health checks that do not know the
    // prefix (the container image's). Other paths are not found.
    Router::new()
        .nest_service(&base, app)
        .route("/healthz", get(|| async { "ok" }))
        .fallback(|| async { error::ApiError::not_found("no such endpoint") })
        .layer(axum::middleware::from_fn(web::headers))
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
    let app = router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let r = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await;
    for t in tasks {
        t.abort();
    }
    if let Err(e) = state.activity.flush(&*state.store).await {
        tracing::warn!("writing mail counts: {e}");
    }
    if let Some(c) = &state.cluster {
        c.leave(&*state.store).await;
    }
    r
}
