use crate::app::AppState;
use crate::handles::{auth, message, moment, task, user};
use crate::middleware::{cors, jwt, logger};
use axum::{
    Router, middleware,
    extract::DefaultBodyLimit,
    http::{HeaderValue, header::CACHE_CONTROL},
    routing::{get, post, put},
};
use tower::ServiceBuilder;
use tower_http::{
    services::ServeDir,
    set_header::SetResponseHeaderLayer,
};

const MAX_MOMENT_BODY_BYTES: usize = 2 * 1024 * 1024 * 1024 + 16 * 1024 * 1024;

pub fn create_router(state: AppState) -> Router {
    let api = Router::new()
        .merge(auth_api())
        .merge(user_api(state.clone()))
        .merge(message_api(state.clone()))
        .merge(moment_api(state.clone()))
        .merge(task_api(state.clone()));

    Router::new()
        .route("/", get(root))
        .nest_service(
            "/api/assets/avatar",
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ))
                .service(ServeDir::new("src/assets/avatar")),
        )
        .nest_service(
            "/api/assets/image",
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ))
                .service(ServeDir::new("src/assets/image")),
        )
        .nest_service(
            "/api/assets/moment",
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ))
                .service(ServeDir::new("src/assets/moment")),
        )
        .nest("/api", api)
        .layer(middleware::from_fn(logger::logger))
        .layer(cors::cors())
        .with_state(state)
}

async fn root() -> &'static str {
    "Hello, World!"
}

fn auth_api() -> Router<AppState> {
    let public = Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/github/login", get(auth::github_login))
        .route("/auth/github/callback", get(auth::github_callback))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/refresh", post(auth::refresh));   // 等token过期(7 day)会重新分发token
    Router::new().merge(public)
}

fn user_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/users", get(user::list).post(user::create))
        .route("/users/{id}", get(user::get_by_id).put(user::update).delete(user::delete))
        .route("/users/me", get(user::me))
        .route("/user/profile", put(user::profile).layer(DefaultBodyLimit::max(7 * 1024 * 1024)))   // change info
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn task_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/task", get(task::list).post(task::create))
        .route("/task/{id}", get(task::get_by_id).put(task::update).delete(task::delete))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn message_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/message", post(message::send))
        .route("/message/image", post(message::send_image).layer(DefaultBodyLimit::max(12 * 1024 * 1024)))   // send image, testing
        .route("/message/history", get(message::history))       // chat message history
        .route("/message/user_info", get(message::user_info))   // group and contacts
        .route("/message/ws", get(message::websocket))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn moment_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/moment", post(moment::create).get(moment::list).layer(DefaultBodyLimit::max(MAX_MOMENT_BODY_BYTES)))
        .route("/moment/{id}", get(moment::get))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}
