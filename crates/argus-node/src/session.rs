use std::sync::Arc;
use std::time::Duration;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argus_model::LoginSync;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{AppendHeaders, IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;

use crate::db::{self, Db};
use crate::sync::same_secret;
use crate::util::{DAY, HOUR, MINUTE, SECOND, duration_text, now_ms, random_id};
use crate::{App, log, totp};

type Hmac = hkdf::hmac::Hmac<Sha256>;

const COOKIE: &str = "argus_session";
const SECURE_COOKIE: &str = "__Host-argus_session";
const IDLE: i64 = 7 * DAY;
const LIFETIME: i64 = 30 * DAY;
const RENEW: i64 = HOUR;
const PASSWORD: &str = "dashboard_password";
const TOTP: &str = "totp_secret";
const TOTP_PENDING: &str = "totp_pending";
const TOTP_STEP: &str = "totp_step";
const SETUP_UNTIL: &str = "setup_until";
const SETUP_WINDOW: i64 = 15 * MINUTE;
const MIN_LENGTH: usize = 10;
const KEY: &str = "session_key";
const CHANGED: &str = "login_changed";
const FREE_ATTEMPTS: u32 = 5;
const FIRST_WAIT: i64 = 30 * SECOND;
const MAX_WAIT: i64 = 15 * MINUTE;
const FORGET_AFTER: i64 = HOUR;

#[derive(Debug, Default)]
pub struct Throttle {
    failures: u32,
    last: i64,
    until: i64,
}

impl Throttle {
    fn attempt(&mut self, now: i64) -> Result<(), i64> {
        if now < self.until {
            return Err(self.until - now);
        }
        if now - self.last >= FORGET_AFTER {
            self.failures = 0;
        }
        self.failures += 1;
        self.last = now;
        if self.failures >= FREE_ATTEMPTS {
            let doublings = (self.failures - FREE_ATTEMPTS).min(16);
            self.until = now + (FIRST_WAIT << doublings).min(MAX_WAIT);
        }
        Ok(())
    }

    fn forgive(&mut self) {
        *self = Self::default();
    }
}

struct Session {
    started: i64,
    expires: i64,
}

fn secure(headers: &HeaderMap) -> bool {
    headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https")
}

fn cookie_name(secure: bool) -> &'static str {
    if secure { SECURE_COOKIE } else { COOKIE }
}

fn cookie(secure: bool, value: &str, max_age: i64) -> String {
    let flags = if secure { "; Secure" } else { "" };
    format!("{}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{flags}", cookie_name(secure), max_age.max(0) / SECOND)
}

fn session_id(headers: &HeaderMap) -> Option<String> {
    let prefix = format!("{}=", cookie_name(secure(headers)));
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(prefix.as_str()))
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

fn issue(db: &Db, started: i64, now: i64) -> (String, i64) {
    let expires = (now + IDLE).min(started + LIFETIME);
    let payload = format!("{started}.{expires}.{}", random_id(24));
    (format!("{payload}.{}", sign(&key(db), &payload)), expires)
}

fn check(db: &Db, token: &str, now: i64) -> Option<Session> {
    let (payload, mac) = token.rsplit_once('.')?;
    let mut parts = payload.splitn(3, '.');
    let started = parts.next()?.parse().ok()?;
    let expires = parts.next()?.parse().ok()?;
    parts.next()?;
    (expires > now && has_password(db) && same_secret(mac, &sign(&key(db), payload))).then_some(Session { started, expires })
}

fn current(app: &App, headers: &HeaderMap, now: i64) -> Option<Session> {
    session_id(headers).and_then(|token| check(&app.db(), &token, now))
}

fn renewal(app: &App, session: &Session, secure: bool, now: i64) -> Option<String> {
    if (now + IDLE).min(session.started + LIFETIME) - session.expires < RENEW {
        return None;
    }
    let (token, expires) = issue(&app.db(), session.started, now);
    Some(cookie(secure, &token, expires - now))
}

pub fn export(db: &Db) -> LoginSync {
    LoginSync { password: db::secret(db, PASSWORD), totp: db::secret(db, TOTP), key: key(db), changed: db::meta(db, CHANGED) }
}

pub fn adopt(db: &Db, login: &LoginSync) -> rusqlite::Result<bool> {
    if login.key.is_empty() || export(db) == *login {
        return Ok(false);
    }
    for (name, value) in [(PASSWORD, &login.password), (TOTP, &login.totp)] {
        match value {
            Some(value) => db::set_secret(db, name, value)?,
            None => db::delete_secret(db, name)?,
        }
    }
    db::set_secret(db, KEY, &login.key)?;
    db::set_meta(db, CHANGED, login.changed)?;
    Ok(true)
}

fn wait(ms: i64) -> Response {
    let seconds = (ms + SECOND - 1) / SECOND;
    let text = format!("too many attempts, try again in {}", duration_text(seconds * SECOND));
    (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, seconds.to_string())], text).into_response()
}

fn setup_open(app: &App, now: i64) -> bool {
    stored(app).is_none() && now < db::meta(&app.db(), SETUP_UNTIL)
}

pub fn open_setup(db: &Db, now: i64) -> rusqlite::Result<()> {
    db::set_meta(db, SETUP_UNTIL, now + SETUP_WINDOW)
}

pub fn reset(db: &Db, now: i64) -> rusqlite::Result<()> {
    for name in [PASSWORD, TOTP, TOTP_PENDING] {
        db::delete_secret(db, name)?;
    }
    rotate(db, now)?;
    open_setup(db, now)
}

pub fn has_password(db: &Db) -> bool {
    db::secret(db, PASSWORD).is_some()
}

pub async fn enabled(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if app.config().ui { next.run(request).await } else { StatusCode::NOT_FOUND.into_response() }
}

pub async fn require(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let now = now_ms();
    let secure = secure(request.headers());
    let Some(session) = current(&app, request.headers(), now) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let mut response = next.run(request).await;
    if !response.headers().contains_key(header::SET_COOKIE)
        && let Some(renewed) = renewal(&app, &session, secure, now).and_then(|c| HeaderValue::from_str(&c).ok())
    {
        response.headers_mut().insert(header::SET_COOKIE, renewed);
    }
    response
}

pub async fn status(State(app): State<Arc<App>>, headers: HeaderMap) -> Json<Value> {
    let now = now_ms();
    let state = match (stored(&app).is_some(), follower(&app), setup_open(&app, now)) {
        (true, _, _) => "login",
        (false, true, _) => "pending",
        (false, false, true) => "setup",
        (false, false, false) => "expired",
    };
    let two_factor = db::secret(&app.db(), TOTP).is_some();
    Json(json!({ "state": state, "authenticated": current(&app, &headers, now).is_some(), "two_factor": two_factor }))
}

#[derive(Deserialize)]
pub struct Login {
    password: String,
    #[serde(default)]
    code: String,
}

#[derive(Deserialize)]
pub struct Change {
    current: String,
    password: String,
    #[serde(default)]
    code: String,
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

async fn wrong(app: &App, password: &str, code: &str, totp: Option<&str>, rejected: StatusCode) -> Option<Response> {
    let Some(stored) = stored(app) else { return Some((StatusCode::CONFLICT, "set a password first").into_response()) };
    if let Err(left) = app.logins().attempt(now_ms()) {
        return Some(wait(left));
    }
    let known = verify(app, &stored, password).await;
    let step = match totp {
        Some(secret) => totp::accept(secret, code, now_ms(), db::meta(&app.db(), TOTP_STEP)),
        None => Some(0),
    };
    match (known, step) {
        (true, Some(step)) => {
            if totp.is_some() {
                let _ = db::set_meta(&app.db(), TOTP_STEP, step);
            }
            app.logins().forgive();
            None
        }
        _ => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Some((rejected, if totp.is_some() { "wrong password or code" } else { "wrong password" }).into_response())
        }
    }
}

fn weak(password: &str) -> Option<Response> {
    (password.chars().count() < MIN_LENGTH).then(|| (StatusCode::BAD_REQUEST, "use at least 10 characters").into_response())
}

fn start_session(app: &App, headers: &HeaderMap) -> Response {
    let now = now_ms();
    let (token, expires) = issue(&app.db(), now, now);
    ([(header::SET_COOKIE, cookie(secure(headers), &token, expires - now))], StatusCode::NO_CONTENT).into_response()
}

fn elsewhere(app: &App, what: &str) -> Option<Response> {
    follower(app).then(|| (StatusCode::FORBIDDEN, format!("{what} on the internal node")).into_response())
}

pub async fn setup(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<Login>) -> Response {
    if let Some(refused) = elsewhere(&app, "set the password") {
        return refused;
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
    if let Some(refused) = elsewhere(&app, "change the password") {
        return refused;
    }
    let totp = db::secret(&app.db(), TOTP);
    if let Some(rejected) = wrong(&app, &form.current, &form.code, totp.as_deref(), StatusCode::FORBIDDEN).await {
        return rejected;
    }
    if let Some(rejected) = weak(&form.password) {
        return rejected;
    }
    let Some(hashed) = hash(&app, form.password).await else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let saved = {
        let db = app.db();
        db::set_secret(&db, PASSWORD, &hashed).and_then(|_| rotate(&db, now_ms()))
    };
    if saved.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    log("dashboard password changed, all sessions ended");
    start_session(&app, &headers)
}

pub async fn login(State(app): State<Arc<App>>, headers: HeaderMap, Json(login): Json<Login>) -> Response {
    let totp = db::secret(&app.db(), TOTP);
    if let Some(rejected) = wrong(&app, &login.password, &login.code, totp.as_deref(), StatusCode::UNAUTHORIZED).await {
        return rejected;
    }
    start_session(&app, &headers)
}

pub async fn totp_start(State(app): State<Arc<App>>) -> Response {
    if let Some(refused) = elsewhere(&app, "set up two-factor sign-in") {
        return refused;
    }
    if db::secret(&app.db(), TOTP).is_some() {
        return (StatusCode::CONFLICT, "two-factor sign-in is already on").into_response();
    }
    let secret = totp::secret();
    if db::set_secret(&app.db(), TOTP_PENDING, &secret).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let uri = totp::uri(&secret);
    ([(header::CACHE_CONTROL, "no-store")], Json(json!({ "secret": secret, "qr": totp::qr(&uri), "uri": uri }))).into_response()
}

pub async fn totp_enable(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<Login>) -> Response {
    if let Some(refused) = elsewhere(&app, "set up two-factor sign-in") {
        return refused;
    }
    let Some(pending) = db::secret(&app.db(), TOTP_PENDING) else {
        return (StatusCode::CONFLICT, "start the setup again").into_response();
    };
    if let Some(rejected) = wrong(&app, &form.password, &form.code, Some(&pending), StatusCode::FORBIDDEN).await {
        return rejected;
    }
    let saved = {
        let db = app.db();
        db::set_secret(&db, TOTP, &pending).and_then(|_| db::delete_secret(&db, TOTP_PENDING)).and_then(|_| rotate(&db, now_ms()))
    };
    if saved.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    log("two-factor sign-in turned on, all sessions ended");
    start_session(&app, &headers)
}

pub async fn totp_disable(State(app): State<Arc<App>>, headers: HeaderMap, Json(form): Json<Login>) -> Response {
    if let Some(refused) = elsewhere(&app, "turn off two-factor sign-in") {
        return refused;
    }
    let Some(secret) = db::secret(&app.db(), TOTP) else {
        return (StatusCode::CONFLICT, "two-factor sign-in is already off").into_response();
    };
    if let Some(rejected) = wrong(&app, &form.password, &form.code, Some(&secret), StatusCode::FORBIDDEN).await {
        return rejected;
    }
    let saved = {
        let db = app.db();
        db::delete_secret(&db, TOTP).and_then(|_| rotate(&db, now_ms()))
    };
    if saved.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    log("two-factor sign-in turned off, all sessions ended");
    start_session(&app, &headers)
}

pub async fn logout() -> Response {
    (AppendHeaders([(header::SET_COOKIE, cookie(false, "", 0)), (header::SET_COOKIE, cookie(true, "", 0))]), StatusCode::NO_CONTENT).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn app() -> App {
        App::new(Config::default(), db::open(":memory:").expect("db"))
    }

    fn with_cookie(value: &str, https: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(value).expect("header"));
        if https {
            headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        }
        headers
    }

    fn valid(app: &App, headers: &HeaderMap) -> bool {
        current(app, headers, now_ms()).is_some()
    }

    #[test]
    fn sessions_are_signed_and_need_a_stored_password() {
        let app = app();
        let (token, _) = issue(&app.db(), now_ms(), now_ms());
        assert!(!valid(&app, &with_cookie(&format!("argus_session={token}"), false)));
        db::create_secret(&app.db(), PASSWORD, "hash").expect("store");
        assert!(valid(&app, &with_cookie(&format!("theme=dark; argus_session={token}"), false)));
        let forged = format!("{}{}", &token[..token.len() - 1], if token.ends_with('0') { '1' } else { '0' });
        assert!(!valid(&app, &with_cookie(&format!("argus_session={forged}"), false)));
        let (idle, _) = issue(&app.db(), now_ms() - IDLE - 1, now_ms() - IDLE - 1);
        assert!(!valid(&app, &with_cookie(&format!("argus_session={idle}"), false)));
        let old_format = format!("{}.{}", now_ms() + DAY, random_id(24));
        assert!(!valid(&app, &with_cookie(&format!("argus_session={old_format}.{}", sign(&key(&app.db()), &old_format)), false)));
        reset(&app.db(), now_ms()).expect("reset");
        db::create_secret(&app.db(), PASSWORD, "hash").expect("store");
        assert!(!valid(&app, &with_cookie(&format!("argus_session={token}"), false)));
    }

    #[test]
    fn https_sessions_only_use_the_host_prefixed_cookie() {
        let app = app();
        db::create_secret(&app.db(), PASSWORD, "hash").expect("store");
        let (token, expires) = issue(&app.db(), now_ms(), now_ms());
        assert!(valid(&app, &with_cookie(&format!("__Host-argus_session={token}"), true)));
        assert!(!valid(&app, &with_cookie(&format!("argus_session={token}"), true)));
        let set = cookie(true, &token, expires - now_ms());
        assert!(set.starts_with("__Host-argus_session=") && set.contains("; Path=/; HttpOnly; SameSite=Strict;") && set.ends_with("; Secure"));
        assert!(!cookie(false, &token, 0).contains("Secure"));
    }

    #[test]
    fn active_sessions_slide_until_the_lifetime_ends() {
        let app = app();
        db::create_secret(&app.db(), PASSWORD, "hash").expect("store");
        let started = 1_000_000;
        let (token, _) = issue(&app.db(), started, started);
        let session = check(&app.db(), &token, started + 1).expect("valid");
        assert!(renewal(&app, &session, false, started + RENEW - 1).is_none());
        let renewed = renewal(&app, &session, false, started + 6 * DAY).expect("renewed");
        let token = renewed.split(';').next().and_then(|c| c.strip_prefix("argus_session=")).expect("value");
        let session = check(&app.db(), token, started + 12 * DAY).expect("still valid after the first idle window");
        assert_eq!(session.started, started);
        let near_end = started + LIFETIME - 2 * DAY;
        let (late, _) = issue(&app.db(), started, near_end);
        let session = check(&app.db(), &late, near_end).expect("valid");
        assert_eq!(session.expires, started + LIFETIME);
        assert!(renewal(&app, &session, false, near_end + RENEW).is_none());
        assert!(check(&app.db(), &late, started + LIFETIME).is_none());
    }

    #[test]
    fn the_external_node_mirrors_the_login_of_the_internal_node() {
        let internal = app();
        let external = app();
        db::create_secret(&internal.db(), PASSWORD, "hash").expect("store");
        db::set_secret(&internal.db(), TOTP, &totp::secret()).expect("totp");
        rotate(&internal.db(), 5).expect("rotate");
        let (token, _) = issue(&internal.db(), now_ms(), now_ms());
        assert!(adopt(&external.db(), &export(&internal.db())).expect("adopt"));
        assert!(!adopt(&external.db(), &export(&internal.db())).expect("again"));
        assert!(check(&external.db(), &token, now_ms()).is_some());
        assert_eq!(db::secret(&external.db(), TOTP), db::secret(&internal.db(), TOTP));
        reset(&internal.db(), 10).expect("reset");
        adopt(&external.db(), &export(&internal.db())).expect("adopt");
        assert!(check(&external.db(), &token, now_ms()).is_none());
        assert!(db::secret(&external.db(), PASSWORD).is_none());
        assert!(db::secret(&external.db(), TOTP).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn changing_the_password_ends_old_sessions_without_hanging() {
        let app = Arc::new(app());
        let first = hash(&app, "first-password".into()).await.expect("hash");
        db::create_secret(&app.db(), PASSWORD, &first).expect("store");
        let (token, _) = issue(&app.db(), now_ms(), now_ms());
        let form = Change { current: "first-password".into(), password: "second-password".into(), code: String::new() };
        let call = tokio::spawn(change(State(app.clone()), HeaderMap::new(), Json(form)));
        let response = tokio::time::timeout(Duration::from_secs(60), call).await.expect("change returns").expect("task");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(check(&app.db(), &token, now_ms()).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_factor_sign_in_needs_a_fresh_code() {
        let app = Arc::new(app());
        let hashed = hash(&app, "first-password".into()).await.expect("hash");
        db::create_secret(&app.db(), PASSWORD, &hashed).expect("store");
        let started = totp_start(State(app.clone())).await;
        assert_eq!(started.status(), StatusCode::OK);
        let pending = db::secret(&app.db(), TOTP_PENDING).expect("pending");
        let form = |password: &str, code: String| Json(Login { password: password.into(), code });
        let first = totp::current_code(&pending, now_ms());
        let enabled = totp_enable(State(app.clone()), HeaderMap::new(), form("first-password", first.clone())).await;
        assert_eq!(enabled.status(), StatusCode::NO_CONTENT);
        assert_eq!(db::secret(&app.db(), TOTP).as_deref(), Some(pending.as_str()));
        let reused = login(State(app.clone()), HeaderMap::new(), form("first-password", first)).await;
        assert_eq!(reused.status(), StatusCode::UNAUTHORIZED);
        let missing = login(State(app.clone()), HeaderMap::new(), form("first-password", String::new())).await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        db::set_meta(&app.db(), TOTP_STEP, 0).expect("forget the step");
        let fresh = login(State(app.clone()), HeaderMap::new(), form("first-password", totp::current_code(&pending, now_ms()))).await;
        assert_eq!(fresh.status(), StatusCode::NO_CONTENT);
        let disabled = totp_disable(State(app.clone()), HeaderMap::new(), form("first-password", String::new())).await;
        assert_eq!(disabled.status(), StatusCode::FORBIDDEN);
        assert!(db::secret(&app.db(), TOTP).is_some());
    }

    #[test]
    fn wrong_attempts_make_the_next_one_wait_longer() {
        let mut throttle = Throttle::default();
        for _ in 1..FREE_ATTEMPTS {
            assert!(throttle.attempt(1_000).is_ok());
        }
        assert!(throttle.attempt(1_000).is_ok());
        assert_eq!(throttle.attempt(1_000), Err(FIRST_WAIT));
        assert!(throttle.attempt(1_000 + FIRST_WAIT).is_ok());
        assert_eq!(throttle.attempt(1_000 + FIRST_WAIT), Err(2 * FIRST_WAIT));
        for _ in 0..20 {
            let free = throttle.until;
            assert!(throttle.attempt(free).is_ok());
        }
        assert_eq!(throttle.until - throttle.last, MAX_WAIT);
        throttle.forgive();
        assert!(throttle.attempt(throttle.until).is_ok());
        let mut quiet = Throttle { failures: 3, last: 0, until: 0 };
        assert!(quiet.attempt(FORGET_AFTER).is_ok());
        assert_eq!(quiet.failures, 1);
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
