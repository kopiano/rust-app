use axum::http::{
    HeaderName, HeaderValue, Method,
    header::{
        ACCEPT, ACCEPT_RANGES, AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
        USER_AGENT,
    },
};
use tower_http::cors::CorsLayer;

pub fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin([
            "http://localhost:3000".parse::<HeaderValue>().unwrap(),
            "http://127.0.0.1:3000".parse::<HeaderValue>().unwrap(),
            "https://www.coulsonzero.shop".parse::<HeaderValue>().unwrap(),
            "https://kopiano.cc".parse::<HeaderValue>().unwrap(),
        ])
        .allow_headers([AUTHORIZATION, ACCEPT, CONTENT_TYPE, USER_AGENT, RANGE, HeaderName::from_static("upload-offset")])
        .expose_headers([ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, HeaderName::from_static("upload-offset")])
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH, Method::DELETE, Method::OPTIONS])
}
