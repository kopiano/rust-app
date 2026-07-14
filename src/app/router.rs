use crate::app::AppState;
use crate::handles::{auth, task, user};
use crate::middleware::{cors, jwt, logger};
use axum::{
    Router, middleware,
    routing::{get, post},
};

pub fn create_router(state: AppState) -> Router {
    let api = Router::new()
        .merge(auth_api())
        .merge(user_api(state.clone()))
        .merge(task_api(state.clone()));

    Router::new()
        .route("/", get(root))
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
        .route("/auth/refresh", post(auth::refresh));
    Router::new().merge(public)
}

fn user_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/users", get(user::list).post(user::create))
        .route("/users/{id}", get(user::get_by_id).put(user::update).delete(user::delete))
        .route("/users/me", get(user::me))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn task_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/task", get(task::list).post(task::create))
        .route("/task/{id}", get(task::get_by_id).put(task::update).delete(task::delete))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}
