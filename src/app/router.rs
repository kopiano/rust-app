use axum::{Router, middleware, routing::{get, post}};
use crate::app::AppState;
use crate::handles::{auth, task, user};
use crate::middleware::{cors, jwt, logger};

pub fn create_router(state: AppState) -> Router {
    let api = Router::new()
        .merge(auth_api(state.clone()))
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

fn auth_api(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login));

    let protected = Router::new()
        .route("/auth/me", get(auth::me))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth));

    Router::new().merge(public).merge(protected)
}

fn user_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/user", get(user::list).post(user::create))
        .route("/user/{id}", get(user::get_by_id).put(user::update).delete(user::delete))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn task_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/task", get(task::list).post(task::create))
        .route("/task/{id}", get(task::get_by_id).put(task::update).delete(task::delete))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}


