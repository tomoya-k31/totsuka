use axum::{extract::Request, middleware::Next, response::Response};
use uuid::Uuid;

pub const HEADER: &str = "x-totsuka-request-id";

/// 着信時に request-id を取得、なければ生成。response にも echo
pub async fn middleware(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    req.extensions_mut().insert(RequestId(id.clone()));
    let mut res = next.run(req).await;
    res.headers_mut().insert(HEADER, id.parse().unwrap());
    res
}

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, middleware, routing::get, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/ping", get(|| async { "pong" }))
            .layer(middleware::from_fn(super::middleware))
    }

    #[tokio::test]
    async fn missing_header_generates_uuid() {
        let res = app()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let val = res.headers().get(HEADER).expect("header present");
        let s = val.to_str().unwrap();
        // must be a valid UUID v4
        assert!(uuid::Uuid::parse_str(s).is_ok(), "not a UUID: {s}");
    }

    #[tokio::test]
    async fn provided_header_echoed_back() {
        let res = app()
            .oneshot(
                Request::builder()
                    .uri("/ping")
                    .header(HEADER, "my-custom-id-42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let val = res.headers().get(HEADER).expect("header present");
        assert_eq!(val.to_str().unwrap(), "my-custom-id-42");
    }
}
