use std::sync::Arc;
use std::time::Duration;

use argon2::password_hash::{PasswordHash, PasswordVerifier};
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;
use crate::util::{DAY, now_ms, random_id};

const COOKIE: &str = "argus_session";
const LIFETIME: i64 = 30 * DAY;

fn session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix("argus_session="))
        .map(str::to_string)
}

pub fn valid(app: &App, headers: &HeaderMap) -> bool {
    if app.config().dashboard.is_none() {
        return true;
    }
    let Some(id) = session_id(headers) else {
        return false;
    };
    let now = now_ms();
    let mut sessions = app.sessions();
    sessions.retain(|_, expires| *expires > now);
    sessions.contains_key(&id)
}

pub async fn enabled(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if app.config().ui { next.run(request).await } else { StatusCode::NOT_FOUND.into_response() }
}

pub async fn require(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if valid(&app, request.headers()) { next.run(request).await } else { (StatusCode::UNAUTHORIZED, "login required").into_response() }
}

pub async fn status(State(app): State<Arc<App>>, headers: HeaderMap) -> Json<Value> {
    Json(json!({ "required": app.config().dashboard.is_some(), "authenticated": valid(&app, &headers) }))
}

#[derive(Deserialize)]
pub struct Login {
    password: String,
}

pub async fn verify(app: &App, hash: &str, password: &str) -> bool {
    let Ok(_permit) = app.argon.acquire().await else {
        return false;
    };
    let (hash, password) = (hash.to_string(), password.to_string());
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|parsed| argon2::Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
    })
    .await
    .unwrap_or(false)
}

pub async fn login(State(app): State<Arc<App>>, headers: HeaderMap, Json(login): Json<Login>) -> Response {
    let Some(dashboard) = app.config().dashboard.clone() else {
        return StatusCode::NO_CONTENT.into_response();
    };
    if !verify(&app, &dashboard.password, &login.password).await {
        tokio::time::sleep(Duration::from_secs(1)).await;
        return (StatusCode::UNAUTHORIZED, "wrong password").into_response();
    }
    let id = random_id(48);
    app.sessions().insert(id.clone(), now_ms() + LIFETIME);
    let secure = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    let cookie = format!("{COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}", LIFETIME / 1_000, if secure { "; Secure" } else { "" });
    ([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response()
}

pub async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(id) = session_id(&headers) {
        app.sessions().remove(&id);
    }
    ([(header::SET_COOKIE, format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"))], StatusCode::NO_CONTENT).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db;
    use axum::http::HeaderValue;

    fn app(password: bool) -> App {
        let mut config = Config::default();
        if password {
            config.dashboard = Some(crate::config::Dashboard { password: "x".into() });
        }
        App::new(config, db::open(":memory:").expect("db"))
    }

    fn with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(value).expect("header"));
        headers
    }

    #[test]
    fn open_without_password_and_closed_without_session() {
        assert!(valid(&app(false), &HeaderMap::new()));
        let protected = app(true);
        assert!(!valid(&protected, &HeaderMap::new()));
        protected.sessions().insert("abc".into(), now_ms() + 1_000);
        assert!(valid(&protected, &with_cookie("theme=dark; argus_session=abc")));
        assert!(!valid(&protected, &with_cookie("argus_session=other")));
        protected.sessions().insert("old".into(), now_ms() - 1);
        assert!(!valid(&protected, &with_cookie("argus_session=old")));
    }
}
