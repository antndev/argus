use std::sync::Arc;
use std::time::Duration;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argus_model::LoginSync;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;

use crate::db::{self, Db};
use crate::sync::same_secret;
use crate::util::{DAY, MINUTE, now_ms, random_id};
use crate::{App, log};

type Hmac = hkdf::hmac::Hmac<Sha256>;

const COOKIE: &str = "argus_session";
const LIFETIME: i64 = 30 * DAY;
const PASSWORD: &str = "dashboard_password";
const SETUP_UNTIL: &str = "setup_until";
const SETUP_WINDOW: i64 = 15 * MINUTE;
const MIN_LENGTH: usize = 10;
const KEY: &str = "session_key";
const CHANGED: &str = "login_changed";
const FAILURE_WINDOW: i64 = 10 * MINUTE;
const MAX_FAILURES: u32 = 20;

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

fn follower(app: &App) -> bool {
    let config = app.config();
    config.peer.is_some() && !config.initiates_sync()
}

fn key(db: &Db) -> String {
    if let Some(key) = db::secret(db, KEY) {
        return key;
    }
    let _ = db::create_secret(db, KEY, &random_id(64));
    db::secret(db, KEY).unwrap_or_default()
}

fn rotate(db: &Db, now: i64) -> rusqlite::Result<()> {
    db::set_secret(db, KEY, &random_id(64))?;
    db::set_meta(db, CHANGED, now)
}

fn sign(key: &str, payload: &str) -> String {
    use hkdf::hmac::Mac;
    let mut mac = <Hmac as Mac>::new_from_slice(key.as_bytes()).expect("hmac takes any key length");
    mac.update(payload.as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn issue(db: &Db, now: i64) -> String {
    let payload = format!("{}.{}", now + LIFETIME, random_id(24));
    format!("{payload}.{}", sign(&key(db), &payload))
}

fn check(db: &Db, token: &str, now: i64) -> bool {
    let Some((payload, mac)) = token.rsplit_once('.') else { return false };
    let expires = payload.split('.').next().and_then(|e| e.parse::<i64>().ok()).unwrap_or(0);
    expires > now && db::secret(db, PASSWORD).is_some() && same_secret(mac, &sign(&key(db), payload))
}

pub fn export(db: &Db) -> LoginSync {
    LoginSync { password: db::secret(db, PASSWORD), key: key(db), changed: db::meta(db, CHANGED) }
}

pub fn adopt(db: &Db, login: &LoginSync) -> rusqlite::Result<bool> {
    if login.key.is_empty() || export(db) == *login {
        return Ok(false);
    }
    match &login.password {
        Some(hash) => db::set_secret(db, PASSWORD, hash)?,
        None => db::delete_secret(db, PASSWORD)?,
    }
    db::set_secret(db, KEY, &login.key)?;
    db::set_meta(db, CHANGED, login.changed)?;
    Ok(true)
}

fn throttled(state: &(i64, u32), now: i64) -> bool {
    now - state.0 < FAILURE_WINDOW && state.1 >= MAX_FAILURES
}

fn failed(state: &mut (i64, u32), now: i64) {
    if now - state.0 >= FAILURE_WINDOW {
        *state = (now, 0);
    }
    state.1 += 1;
}

fn too_many() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, "too many wrong passwords, wait a few minutes").into_response()
}

fn setup_open(app: &App, now: i64) -> bool {
    stored(app).is_none() && now < db::meta(&app.db(), SETUP_UNTIL)
}

pub fn open_setup(db: &Db, now: i64) -> rusqlite::Result<()> {
    db::set_meta(db, SETUP_UNTIL, now + SETUP_WINDOW)
}

pub fn reset(db: &Db, now: i64) -> rusqlite::Result<()> {
    db::delete_secret(db, PASSWORD)?;
    rotate(db, now)?;
    open_setup(db, now)
}

pub fn has_password(db: &Db) -> bool {
    db::secret(db, PASSWORD).is_some()
}

pub fn valid(app: &App, headers: &HeaderMap) -> bool {
    session_id(headers).is_some_and(|token| check(&app.db(), &token, now_ms()))
}

pub async fn enabled(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if app.config().ui { next.run(request).await } else { StatusCode::NOT_FOUND.into_response() }
}

pub async fn require(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if valid(&app, request.headers()) { next.run(request).await } else { (StatusCode::UNAUTHORIZED, "login required").into_response() }
}

pub async fn status(State(app): State<Arc<App>>, headers: HeaderMap) -> Json<Value> {
    let state = match (stored(&app).is_some(), follower(&app), setup_open(&app, now_ms())) {
        (true, _, _) => "login",
        (false, true, _) => "pending",
        (false, false, true) => "setup",
        (false, false, false) => "expired",
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
    let id = issue(&app.db(), now_ms());
    let secure = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    let cookie = format!("{COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}", LIFETIME / 1_000, if secure { "; Secure" } else { "" });
    ([(header::SET_COOKIE, cookie)], StatusCode::NO_CONTENT).into_response()
}

pub async fn setup(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<Login>) -> Response {
    if follower(&app) {
        return (StatusCode::FORBIDDEN, "set the password on the internal node").into_response();
    }
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
    if rotate(&app.db(), now_ms()).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    log("dashboard password set");
    start_session(&app, &headers)
}

pub async fn change(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<Change>) -> Response {
    if follower(&app) {
        return (StatusCode::FORBIDDEN, "change the password on the internal node").into_response();
    }
    let Some(current) = stored(&app) else { return StatusCode::UNAUTHORIZED.into_response() };
    if throttled(&app.logins(), now_ms()) {
        return too_many();
    }
    if !verify(&app, &current, &form.current).await {
        failed(&mut app.logins(), now_ms());
        tokio::time::sleep(Duration::from_secs(1)).await;
        return (StatusCode::FORBIDDEN, "the current password is wrong").into_response();
    }
    if let Some(rejected) = weak(&form.password) {
        return rejected;
    }
    let Some(hashed) = hash(&app, form.password).await else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    if db::set_secret(&app.db(), PASSWORD, &hashed).and_then(|_| rotate(&app.db(), now_ms())).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    log("dashboard password changed, all sessions ended");
    start_session(&app, &headers)
}

pub async fn login(State(app): State<Arc<App>>, headers: HeaderMap, Json(login): Json<Login>) -> Response {
    let Some(stored) = stored(&app) else { return (StatusCode::CONFLICT, "set a password first").into_response() };
    if throttled(&app.logins(), now_ms()) {
        return too_many();
    }
    if !verify(&app, &stored, &login.password).await {
        failed(&mut app.logins(), now_ms());
        tokio::time::sleep(Duration::from_secs(1)).await;
        return (StatusCode::UNAUTHORIZED, "wrong password").into_response();
    }
    start_session(&app, &headers)
}

pub async fn logout() -> Response {
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
    fn sessions_are_signed_and_need_a_stored_password() {
        let app = app();
        let token = issue(&app.db(), now_ms());
        assert!(!valid(&app, &with_cookie(&format!("argus_session={token}"))));
        db::create_secret(&app.db(), PASSWORD, "hash").expect("store");
        assert!(valid(&app, &with_cookie(&format!("theme=dark; argus_session={token}"))));
        let forged = format!("{}0", &token[..token.len() - 1]);
        assert!(!valid(&app, &with_cookie(&format!("argus_session={forged}"))));
        let old = issue(&app.db(), now_ms() - LIFETIME - 1);
        assert!(!valid(&app, &with_cookie(&format!("argus_session={old}"))));
        reset(&app.db(), now_ms()).expect("reset");
        db::create_secret(&app.db(), PASSWORD, "hash").expect("store");
        assert!(!valid(&app, &with_cookie(&format!("argus_session={token}"))));
    }

    #[test]
    fn the_external_node_mirrors_the_login_of_the_internal_node() {
        let internal = app();
        let external = app();
        db::create_secret(&internal.db(), PASSWORD, "hash").expect("store");
        rotate(&internal.db(), 5).expect("rotate");
        let token = issue(&internal.db(), now_ms());
        assert!(adopt(&external.db(), &export(&internal.db())).expect("adopt"));
        assert!(!adopt(&external.db(), &export(&internal.db())).expect("again"));
        assert!(check(&external.db(), &token, now_ms()));
        reset(&internal.db(), 10).expect("reset");
        adopt(&external.db(), &export(&internal.db())).expect("adopt");
        assert!(!check(&external.db(), &token, now_ms()));
        assert!(db::secret(&external.db(), PASSWORD).is_none());
    }

    #[test]
    fn wrong_passwords_lock_for_a_while() {
        let mut state = (0, 0);
        for _ in 0..MAX_FAILURES {
            assert!(!throttled(&state, 1_000));
            failed(&mut state, 1_000);
        }
        assert!(throttled(&state, 1_000));
        assert!(!throttled(&state, 1_000 + FAILURE_WINDOW));
        failed(&mut state, 1_000 + FAILURE_WINDOW);
        assert_eq!(state.1, 1);
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
