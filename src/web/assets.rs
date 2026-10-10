//! The stylesheet and script, compiled into the binary from `assets/`. In debug builds they are
//! read from disk on each request, so editing them needs no rebuild.

use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

pub(super) async fn serve(Path(file): Path<String>) -> Response {
    let Some(asset) = Assets::get(&file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match file.rsplit_once('.').map(|(_, extension)| extension) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    };
    let mut response = asset.data.into_owned().into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}
