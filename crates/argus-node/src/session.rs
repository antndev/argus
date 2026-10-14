use std::sync::Arc;
use std::time::Duration;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::{self, Db};
use crate::util::{DAY, MINUTE, now_ms, random_id};
use crate::{App, log};

const COOKIE: &str = "argus_session";
const LIFETIME: i64 = 30 * DAY;
const PASSWORD: &str = "dashboard_password";
const SETUP_UNTIL: &str = "setup_until";
const SETUP_WINDOW: i64 = 15 * MINUTE;
const MIN_LENGTH: usize = 10;

fn session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix("argus_session="))
        .map(str::to_string)
}

fn stored(app: &App) -> Option<String> {
    db::secret(&app.db(), PASSWORD)
}

fn setup_open(app: &App, now: i64) -> bool {
    stored(app).is_none() && now < db::meta(&app.db(), SETUP_UNTIL)
}

pub fn open_setup(db: &Db, now: i64) -> rusqlite::Result<()> {
    db::set_meta(db, SETUP_UNTIL, now + SETUP_WINDOW)
}

pub fn reset(db: &Db, now: i64) -> rusqlite::Result<()> {
    db::delete_secret(db, PASSWORD)?;
    open_setup(db, now)
}

pub fn has_password(db: &Db) -> bool {
    db::secret(db, PASSWORD).is_some()
}

pub fn valid(app: &App, headers: &HeaderMap) -> bool {
    let Some(id) = session_id(headers) else { return false };
    if stored(app).is_none() {
        return false;
    }
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
    let state = match (stored(&app).is_some(), setup_open(&app, now_ms())) {
        (true, _) => "login",
        (false, true) => "setup",
        (false, false) => "expired",
    };
    Json(json!({ "state": state, "authenticated": valid(&app, &headers) }))
}

#[derive(Deserialize)]
pub struct Login {
    password: String,
}

#[derive(Deserialize)]
pub struct Change {
    current: String,
    password: String,
}

pub async fn verify(app: &App, hash: &str, password: &str) -> bool {
    let Ok(_permit) = app.argon.acquire().await else { return false };
    let (hash, password) = (hash.to_string(), password.to_string());
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|parsed| argon2::Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
    })
    .await
    .unwrap_or(false)
}

async fn hash(app: &App, password: String) -> Option<String> {
    let _permit = app.argon.acquire().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).ok()?;
        let salt = SaltString::encode_b64(&salt).ok()?;
        argon2::Argon2::default().hash_password(password.as_bytes(), &salt).ok().map(|h| h.to_string())
    })
    .await
    .ok()
    .flatten()
}

fn weak(password: &str) -> Option<Response> {
    (password.chars().count() < MIN_LENGTH).then(|| (StatusCode::BAD_REQUEST, "use at least 10 characters").into_response())
}

fn start_session(app: &App, headers: &HeaderMap) -> Response {
    let id = random_id(48);
    app.sessions().insert(id.clone(), now_ms() + LIFETIME);
    let secure = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    let cookie = format!("{COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}", LIFETIME / 1_000, if secure { "; Secure" } else { "" });
    ([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response()
}

pub async fn setup(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<Login>) -> Response {
    if !setup_open(&app, now_ms()) {
        return (StatusCode::FORBIDDEN, "setup is closed").into_response();
    }
    if let Some(rejected) = weak(&form.password) {
        return rejected;
    }
    let Some(hashed) = hash(&app, form.password).await else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    if !db::create_secret(&app.db(), PASSWORD, &hashed).unwrap_or(false) {
        return (StatusCode::CONFLICT, "a password is already set").into_response();
    }
    app.sessions().clear();
    log("dashboard password set");
    start_session(&app, &headers)
}

pub async fn change(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<Change>) -> Response {
    let Some(current) = stored(&app) else { return StatusCode::UNAUTHORIZED.into_response() };
    if !verify(&app, &current, &form.current).await {
        tokio::time::sleep(Duration::from_secs(1)).await;
        return (StatusCode::FORBIDDEN, "the current password is wrong").into_response();
    }
    if let Some(rejected) = weak(&form.password) {
        return rejected;
    }
    let Some(hashed) = hash(&app, form.password).await else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    if db::set_secret(&app.db(), PASSWORD, &hashed).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    app.sessions().clear();
    log("dashboard password changed, all sessions ended");
    start_session(&app, &headers)
}

pub async fn login(State(app): State<Arc<App>>, headers: HeaderMap, Json(login): Json<Login>) -> Response {
    let Some(stored) = stored(&app) else { return (StatusCode::CONFLICT, "set a password first").into_response() };
    if !verify(&app, &stored, &login.password).await {
        tokio::time::sleep(Duration::from_secs(1)).await;
        return (StatusCode::UNAUTHORIZED, "wrong password").into_response();
    }
    start_session(&app, &headers)
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
    use axum::http::HeaderValue;

    fn app() -> App {
        App::new(Config::default(), db::open(":memory:").expect("db"))
    }

    fn with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(value).expect("header"));
        headers
    }

    #[test]
    fn sessions_need_a_stored_password() {
        let app = app();
        app.sessions().insert("abc".into(), now_ms() + 1_000);
        assert!(!valid(&app, &with_cookie("argus_session=abc")));
        db::create_secret(&app.db(), PASSWORD, "hash").expect("store");
        assert!(valid(&app, &with_cookie("theme=dark; argus_session=abc")));
        assert!(!valid(&app, &with_cookie("argus_session=other")));
        app.sessions().insert("old".into(), now_ms() - 1);
        assert!(!valid(&app, &with_cookie("argus_session=old")));
        reset(&app.db(), now_ms()).expect("reset");
        assert!(!valid(&app, &with_cookie("argus_session=abc")));
    }

    #[test]
    fn setup_is_only_open_for_a_while_and_only_once() {
        let app = app();
        assert!(!setup_open(&app, 1_000));
        open_setup(&app.db(), 1_000).expect("open");
        assert!(setup_open(&app, 1_000 + SETUP_WINDOW - 1));
        assert!(!setup_open(&app, 1_000 + SETUP_WINDOW));
        assert!(db::create_secret(&app.db(), PASSWORD, "first").expect("store"));
        assert!(!db::create_secret(&app.db(), PASSWORD, "second").expect("store"));
        assert!(!setup_open(&app, 1_000));
    }
}
