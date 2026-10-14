use axum::{
    body::Body,
    http::{Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

#[derive(rust_embed::RustEmbed)]
#[folder = "web/build/"]
#[allow_missing = true]
struct Assets;

pub fn reserved(path: &str) -> bool {
    ["/api", "/auth", "/webhook", "/healthz"]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

pub async fn serve(method: Method, uri: Uri) -> Response {
    if method != Method::GET || reserved(uri.path()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    page(uri.path())
}

pub fn page(path: &str) -> Response {
    let requested = path.trim_start_matches('/');
    let file = Assets::get(requested)
        .map(|f| (requested, f))
        .or_else(|| Assets::get("index.html").map(|f| ("index.html", f)));
    let Some((name, file)) = file else {
        return (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            "dashboard not built\n",
        )
            .into_response();
    };
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    let cache = if name.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(file.data.into_owned()))
        .unwrap()
}
