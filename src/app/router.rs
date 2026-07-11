use axum::{Router, middleware, routing::{get, post}};
use crate::app::AppState;
use crate::handles::{auth, task, user};
use crate::middleware::{cors, jwt, logger};

pub fn create_router(state: AppState) -> Router {
    let auth_api = Router::new()
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login));

    let user_api = Router::new()
        .route("/api/user", get(user::list).post(user::create))
        .route("/api/user/{id}", get(user::get_by_id).put(user::update).delete(user::delete))
        .route_layer(middleware::from_fn_with_state(state.clone(), jwt::require_auth));

    let task_api = Router::new()
        .route("/api/task", get(task::list).post(task::create))
        .route("/api/task/{id}", get(task::get_by_id).put(task::update).delete(task::delete))
        .route_layer(middleware::from_fn_with_state(state.clone(), jwt::require_auth));

    Router::new()
        .route("/", get(root))
        .merge(auth_api)
        .merge(user_api)
        .merge(task_api)
        .layer(middleware::from_fn(logger::logger))
        .layer(cors::cors())
        .with_state(state)
}

async fn root() -> &'static str {
    "Hello, World!"
}
