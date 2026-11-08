//! The public routes and middleware, outermost first: trace → reject `Origin` → client IP → drain
//! guard → JSON envelope for bare errors → catch panic → the route group's own body limit and
//! timeout (JSON, uploads, media) → the route's per-IP limit → routes. [`table`] is the one place
//! that decides a route's path, group and limit.

use std::{any::Any, sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    extract::{MatchedPath, Request, State},
    http::{StatusCode, header},
    middleware::{Next, from_fn, from_fn_with_state, map_response},
    response::{IntoResponse, Response},
    routing::{MethodRouter, get, patch, post, put},
};
use ipnet::IpNet;
use lobby_protocol::{LobbyInfo, UnixMs, limits, routes};
use serde::Serialize;
use tower_http::{
    catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{Span, debug, error, field::Empty, info, info_span};

use crate::{
    access, account, auth, chat,
    client_ip::{self, ClientIp},
    error::ApiError,
    media, owner, profile_images,
    server::App,
    uploads, ws,
};

const JSON_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Uploads and media stream big bodies.
const BLOB_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// What a route's group decides: its body limit and timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Json,
    Uploads,
    Media,
}

/// How many requests one client IP may make to a route in one
/// [`ROUTE_WINDOW`](crate::limits::ROUTE_WINDOW). A route that must not be limited says so with
/// `Unlimited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Limit {
    PerWindow(usize),
    Unlimited,
}

/// What routes get unless something about them says otherwise, so nothing is unlimited by omission.
const BASELINE: Limit = Limit::PerWindow(120);

struct Route {
    path: &'static str,
    group: Group,
    limit: Limit,
    methods: MethodRouter<App>,
}

/// Every route, with its group and limit. The routes are keyed by the matched path, so each has
/// its own bucket per IP, whatever its methods.
fn table() -> Vec<Route> {
    use Group::{Json, Media, Uploads};
    use Limit::{PerWindow, Unlimited};

    let route = |group, path, limit, methods| Route {
        path,
        group,
        limit,
        methods,
    };
    vec![
        route(Json, routes::HEALTH, Unlimited, get(health)),
        route(Json, routes::LOBBY, PerWindow(60), get(lobby_info)),
        route(
            Json,
            routes::MAGIC_CODE,
            PerWindow(5),
            post(auth::magic::request_code),
        ),
        route(
            Json,
            routes::MAGIC_CODE_VERIFY,
            PerWindow(5),
            post(auth::magic::verify_code),
        ),
        route(Json, routes::JOIN, PerWindow(5), post(auth::join::join)),
        // Waiting for a Vouch checks every 5 s; a LAN party of five waiters shares one IP.
        route(
            Json,
            routes::JOIN_STATUS,
            PerWindow(60),
            post(auth::join::status),
        ),
        route(Json, routes::REFRESH, PerWindow(30), post(auth::refresh)),
        route(Json, routes::SIGN_OUT, BASELINE, post(auth::sign_out)),
        route(
            Json,
            routes::USERS_ME,
            PerWindow(10),
            patch(account::rename).delete(owner::leave),
        ),
        // Scrolling back pages quickly, and friends share one NAT.
        route(Json, routes::MESSAGES, PerWindow(300), get(chat::history)),
        // Once per reconnect of each device.
        route(
            Json,
            routes::MESSAGE_CHANGES,
            PerWindow(60),
            get(chat::changes),
        ),
        route(Json, routes::VOUCH, PerWindow(5), post(access::vouch)),
        route(Json, routes::DECLINE, PerWindow(5), post(access::decline)),
        route(
            Json,
            routes::REMOVE_MEMBER,
            PerWindow(5),
            post(owner::remove),
        ),
        route(
            Json,
            routes::LOBBY_OWNER,
            PerWindow(5),
            post(owner::transfer),
        ),
        route(Json, routes::WS, PerWindow(10), get(ws::upgrade)),
        route(
            Uploads,
            routes::UPLOADS,
            PerWindow(10),
            post(uploads::create),
        ),
        route(
            Uploads,
            routes::USERS_ME_AVATAR,
            PerWindow(10),
            put(profile_images::set_avatar).delete(profile_images::remove_avatar),
        ),
        route(
            Uploads,
            routes::LOBBY_ICON,
            PerWindow(10),
            put(profile_images::set_icon).delete(profile_images::remove_icon),
        ),
        // A history page asks for up to 100 previews, and friends share one NAT.
        route(Media, routes::MEDIA, PerWindow(600), get(media::full)),
        route(
            Media,
            routes::MEDIA_PREVIEW,
            PerWindow(600),
            get(media::preview),
        ),
    ]
}

/// What [`gate`] needs to count one request: the route, its limit and the shared counters.
#[derive(Clone)]
struct Gate {
    app: App,
    path: &'static str,
    limit: usize,
}

/// Refuses the request with `RATE_LIMITED` and a retry time once its client IP is over the route's
/// limit. It runs before the handler's own extractors, so a request that fails sign-in counts too.
async fn gate(
    State(gate): State<Gate>,
    Extension(client): Extension<ClientIp>,
    req: Request,
    next: Next,
) -> Response {
    let now = tokio::time::Instant::now();
    match gate
        .app
        .limits
        .try_acquire(gate.path, gate.limit, client, now)
    {
        Ok(()) => next.run(req).await,
        Err(retry_at) => {
            debug!(ip = %client.ip(), route = gate.path, "rate limited");
            ApiError::rate_limited("Too many requests", retry_at.saturating_duration_since(now))
                .into_response()
        }
    }
}

/// The routes of one group, each behind its own limit, with the group's body limit and timeout.
fn build_group(app: &App, group: Group, routes: Vec<Route>) -> Router<App> {
    let router = routes.into_iter().fold(Router::new(), |router, r| {
        let methods = match r.limit {
            Limit::PerWindow(limit) => r.methods.layer(from_fn_with_state(
                Gate {
                    app: app.clone(),
                    path: r.path,
                    limit,
                },
                gate,
            )),
            Limit::Unlimited => r.methods,
        };
        router.route(r.path, methods)
    });
    match group {
        Group::Json => router
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                JSON_REQUEST_TIMEOUT,
            ))
            .layer(RequestBodyLimitLayer::new(limits::JSON_BODY_MAX_BYTES)),
        Group::Uploads => {
            // One past the limit: a body of exactly `upload_max_bytes + 1` reaches `save`, which
            // refuses it with our own message; the layer is the backstop for anything bigger.
            let upload_limit =
                usize::try_from(app.blobs.max_bytes().saturating_add(1)).unwrap_or(usize::MAX);
            router
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    BLOB_REQUEST_TIMEOUT,
                ))
                .layer(RequestBodyLimitLayer::new(upload_limit))
        }
        Group::Media => router
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                BLOB_REQUEST_TIMEOUT,
            ))
            .layer(RequestBodyLimitLayer::new(limits::JSON_BODY_MAX_BYTES)),
    }
}

pub(crate) fn router(app: App) -> Router {
    let proxies: Arc<[IpNet]> = app.cfg.http.trusted_proxy_cidrs.clone().into();
    let mut rest = table();
    let mut routed = Router::new();
    for g in [Group::Json, Group::Uploads, Group::Media] {
        let (mine, others) = rest.into_iter().partition(|r| r.group == g);
        rest = others;
        routed = routed.merge(build_group(&app, g, mine));
    }
    routed
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(CatchPanicLayer::custom(handler_panicked))
        .layer(map_response(json_error_body))
        .layer(from_fn_with_state(app.clone(), refuse_while_draining))
        .layer(from_fn_with_state(app.clone(), client_ip::resolve))
        .layer(from_fn(reject_origin))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(move |req: &Request| http_span(req, &proxies))
                .on_response(record_response)
                // `ApiError` already logs every 5xx at the edge.
                .on_failure(()),
        )
        .with_state(app)
}

fn http_span(req: &Request, proxies: &[IpNet]) -> Span {
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map_or(req.uri().path(), MatchedPath::as_str);
    let ip = client_ip::client_ip(client_ip::peer_of(req.extensions()), req.headers(), proxies);
    info_span!(
        "http",
        method = %req.method(),
        path,
        %ip,
        status = Empty,
        latency_ms = Empty
    )
}

/// TraceLayer's default logs responses at `debug`, below the default filter; every request's
/// outcome belongs in the log.
pub(crate) fn record_response(res: &Response, latency: Duration, span: &Span) {
    span.record("status", res.status().as_u16());
    span.record(
        "latency_ms",
        u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
    );
    info!("response");
}

/// A panicking handler answers 500 `INTERNAL_ERROR` instead of dropping the connection.
pub(crate) fn handler_panicked(panic: Box<dyn Any + Send + 'static>) -> Response {
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload");
    error!(panic = message, "handler panicked");
    ApiError::internal().into_response()
}

/// The only client is native and native HTTP never sends `Origin`; a browser always does.
async fn reject_origin(req: Request, next: Next) -> Response {
    if req.headers().contains_key(header::ORIGIN) {
        return ApiError::forbidden("Browser origins are not allowed").into_response();
    }
    next.run(req).await
}

async fn refuse_while_draining(State(app): State<App>, req: Request, next: Next) -> Response {
    if app.conns.is_draining() && req.uri().path() != routes::HEALTH {
        return ApiError::unavailable("Server is shutting down").into_response();
    }
    next.run(req).await
}

/// Errors produced outside our handlers (timeout, body limit, extractor rejections) carry plain
/// text or nothing; every error a client sees has the JSON envelope.
pub(crate) async fn json_error_body(res: Response) -> Response {
    let status = res.status();
    let is_json = res
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
    if is_json || !(status.is_client_error() || status.is_server_error()) {
        return res;
    }
    ApiError::for_status(status).into_response()
}

pub(crate) async fn not_found() -> ApiError {
    ApiError::not_found("Not found")
}

pub(crate) async fn method_not_allowed() -> ApiError {
    ApiError::for_status(StatusCode::METHOD_NOT_ALLOWED)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failing: Vec<&'static str>,
}

/// `/health`: a reader answers and the SFU loop is alive.
async fn health(State(app): State<App>) -> Response {
    let (status, body) = if app.conns.is_draining() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Health {
                status: "draining",
                failing: vec![],
            },
        )
    } else {
        let mut failing = Vec::new();
        if !app.db.ping().await {
            failing.push("db");
        }
        if !app.sfu.is_alive(UnixMs::now()) {
            failing.push("sfu");
        }
        if failing.is_empty() {
            (
                StatusCode::OK,
                Health {
                    status: "ok",
                    failing,
                },
            )
        } else {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Health {
                    status: "degraded",
                    failing,
                },
            )
        }
    };
    (status, Json(body)).into_response()
}

async fn lobby_info(State(app): State<App>) -> Json<LobbyInfo> {
    Json(app.lobby.info())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const UNLIMITED: &[&str] = &[routes::HEALTH];

    #[test]
    fn every_route_is_routed_once_and_limited_unless_it_is_on_the_unlimited_list() {
        let table = table();
        let mut seen = HashSet::new();
        for route in &table {
            assert!(
                seen.insert(route.path),
                "{} is in the table twice",
                route.path
            );
            match route.limit {
                Limit::PerWindow(per_window) => {
                    assert!(per_window > 0, "{} allows no request", route.path);
                    assert!(
                        !UNLIMITED.contains(&route.path),
                        "{} is on the unlimited list but has a limit",
                        route.path
                    );
                }
                Limit::Unlimited => assert!(
                    UNLIMITED.contains(&route.path),
                    "{} has no rate limit and is not on the unlimited list",
                    route.path
                ),
            }
        }
        for path in UNLIMITED {
            assert!(seen.contains(path), "{path} is unlimited but not routed");
        }
    }
}
