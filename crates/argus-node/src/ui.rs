use std::sync::Arc;

use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

use crate::App;

#[derive(Embed)]
#[folder = "../../target/ui"]
#[allow_missing = true]
struct Assets;

fn hashed(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    stem.rsplit_once('-').is_some_and(|(_, hash)| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()))
}

pub async fn serve(State(app): State<Arc<App>>, method: Method, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if !app.config().ui || !matches!(method, Method::GET | Method::HEAD) || path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (path, file) = match Assets::get(path).filter(|_| !path.is_empty()) {
        Some(file) => (path, file),
        None => match Assets::get("index.html") {
            Some(file) => ("index.html", file),
            None => {
                return (StatusCode::NOT_FOUND, "dashboard not built, run trunk build").into_response();
            }
        },
    };
    let cache = if hashed(path) { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mime = file.metadata.mimetype().to_string();
    ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache.to_string())], file.data).into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_trunk_hashed_files_are_immutable() {
        assert!(super::hashed("argus-ui-9b7b72fd1aee4144_bg.wasm") || super::hashed("argus-ui-9b7b72fd1aee4144.js"));
        assert!(super::hashed("charts-0123456789abcdef.js"));
        assert!(!super::hashed("index.html"));
        assert!(!super::hashed("fonts/inter-latin-wght-normal.woff2"));
    }
}
