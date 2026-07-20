use crate::app::AppState;
use crate::handles::{auth, message, moment, music, subscription, task, user, video};
use crate::middleware::{cors, jwt, logger, plan};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, header::CACHE_CONTROL},
    middleware,
    routing::{get, patch, post, put},
};
use tower::ServiceBuilder;
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer};

const MAX_MOMENT_BODY_BYTES: usize = 2 * 1024 * 1024 * 1024 + 16 * 1024 * 1024;
const MAX_MUSIC_BODY_BYTES: usize = 4 * 1024 * 1024 * 1024;
const MAX_VIDEO_BODY_BYTES: usize = 6 * 1024 * 1024 * 1024 + 32 * 1024 * 1024;
const MAX_VIDEO_UPLOAD_CHUNK_BODY_BYTES: usize = 8 * 1024 * 1024 + 64 * 1024;

pub fn create_router(state: AppState) -> Router {
    let api = Router::new()
        .merge(auth_api())
        .merge(user_api(state.clone()))
        .merge(message_api(state.clone()))
        .merge(moment_api(state.clone()))
        .merge(music_api(state.clone()))
        .merge(video_api(state.clone()))
        .merge(subscription_api(state.clone()))
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
        .nest_service(
            "/api/assets/video",
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ))
                .service(ServeDir::new("src/assets/video")),
        )
        .nest_service(
            "/api/assets/music",
            ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ))
                .service(ServeDir::new("src/assets/music")),
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
        .route("/auth/refresh", post(auth::refresh)); // 等token过期(7 day)会重新分发token
    Router::new().merge(public)
}

fn user_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/users", get(user::list).post(user::create))
        .route(
            "/users/{id}",
            get(user::get_by_id).put(user::update).delete(user::delete),
        )
        .route("/users/me", get(user::me))
        .route(
            "/user/profile",
            put(user::profile).layer(DefaultBodyLimit::max(7 * 1024 * 1024)),
        ) // change info
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn task_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/task", get(task::list).post(task::create))
        .route(
            "/task/{id}",
            get(task::get_by_id).put(task::update).delete(task::delete),
        )
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn message_api(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/message", post(message::send))
        .route(
            "/message/image",
            post(message::send_image).layer(DefaultBodyLimit::max(12 * 1024 * 1024)),
        ) // send image, testing
        .route("/message/history", get(message::history)) // chat message history
        .route("/message/user_info", get(message::user_info)) // group and contacts
        .route(
            "/message/group",
            post(message::create_group).layer(DefaultBodyLimit::max(7 * 1024 * 1024)),
        )
        .route(
            "/message/group/{id}/members",
            post(message::add_group_members),
        )
        .route("/message/ws", get(message::websocket))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth))
}

fn moment_api(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/moment", get(moment::list))
        .route("/moment/{id}", get(moment::get))
        .route("/moment/{id}/view", post(moment::view))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            jwt::optional_auth,
        ));

    let authenticated = Router::new()
        .route(
            "/moment",
            post(moment::create).layer(DefaultBodyLimit::max(MAX_MOMENT_BODY_BYTES)),
        )
        .route("/moment/{id}", axum::routing::delete(moment::delete))
        .route(
            "/moment/{id}/like",
            post(moment::like).delete(moment::unlike),
        )
        .route("/moment/{id}/comment", post(moment::comment))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth));
    Router::new().merge(public).merge(authenticated)
}

fn music_api(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/music/list", get(music::public_list))
        .route("/music/{id}", get(music::public_get))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            jwt::optional_auth,
        ));
    let library = Router::new()
        .route("/music", get(music::list))
        .route("/music/library", get(music::library))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            plan::require_library_access,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            jwt::require_auth,
        ));
    let private = Router::new()
        .route(
            "/music/upload",
            post(music::upload).layer(DefaultBodyLimit::max(MAX_MUSIC_BODY_BYTES)),
        )
        .route("/music/ws", get(music::websocket))
        .route("/music/{id}", axum::routing::delete(music::delete))
        .route("/music/{id}/favorite", put(music::favorite))
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth));
    Router::new().merge(public).merge(library).merge(private)
}

fn video_api(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/video", get(video::list))
        .route("/video/categories", get(video::categories))
        .route("/video/collections", get(video::collections))
        .route("/video/{id}", get(video::get))
        .route("/video/{id}/comments", get(video::comments))
        .route("/video/{id}/view", post(video::view))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            jwt::optional_auth,
        ));

    let authenticated = Router::new()
        .route("/video/uploads", post(video::create_upload))
        .route("/video/uploads/{upload_id}", get(video::upload_status))
        .route(
            "/video/uploads/{upload_id}/chunk",
            put(video::upload_chunk)
                .layer(DefaultBodyLimit::max(MAX_VIDEO_UPLOAD_CHUNK_BODY_BYTES)),
        )
        .route(
            "/video/uploads/{upload_id}/complete",
            post(video::complete_upload),
        )
        .route(
            "/video/upload",
            post(video::upload).layer(DefaultBodyLimit::max(MAX_VIDEO_BODY_BYTES)),
        )
        .route(
            "/video/{id}",
            patch(video::update)
                .delete(video::delete)
                .layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/video/{id}/like", post(video::like).delete(video::unlike))
        .route(
            "/video/{id}/favorite",
            post(video::favorite).delete(video::unfavorite),
        )
        .route("/video/{id}/comments", post(video::create_comment))
        .route(
            "/video/comments/{id}/like",
            post(video::like_comment).delete(video::unlike_comment),
        )
        .route("/video/collections", post(video::create_collection))
        .route(
            "/video/collections/{id}",
            patch(video::update_collection).delete(video::delete_collection),
        )
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth));

    Router::new().merge(public).merge(authenticated)
}

fn subscription_api(state: AppState) -> Router<AppState> {
    let authenticated = Router::new()
        .route(
            "/subscription/pro/checkout",
            post(subscription::create_pro_checkout),
        )
        .route(
            "/subscription/orders/{order_no}/confirm",
            post(subscription::confirm_order),
        )
        .route_layer(middleware::from_fn_with_state(state, jwt::require_auth));
    Router::new()
        .route("/subscription/webhook", post(subscription::webhook))
        .merge(authenticated)
}
