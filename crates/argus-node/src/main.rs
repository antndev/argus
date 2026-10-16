mod api;
mod config;
mod db;
mod notify;
mod push;
mod rules;
mod session;
mod sync;
mod ui;
mod util;
mod zerobyte;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use argus_model::{
    Alert, AlertState, CheckResult, CheckStatus, Event, HostSnapshot, Message, Report, SERVICE_PREFIX, SERVICES_HOST, Sample, ServiceStatus, Severity,
    metric_key,
};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};

use crate::config::Config;
use crate::db::Db;
use crate::util::{MINUTE, SECOND, now_ms, random_id, sha256_hex};

pub const PEER_TIMEOUT: i64 = 60 * SECOND;

#[derive(Clone, Default)]
pub struct PeerState {
    pub last_sync: Option<i64>,
    pub cursor: i64,
    pub streams: u32,
    pub links: Vec<String>,
}

pub struct App {
    config: RwLock<Arc<Config>>,
    config_write: Mutex<()>,
    config_stamp: Mutex<Option<SystemTime>>,
    pub config_path: Option<PathBuf>,
    pub boot: Arc<Config>,
    db: Mutex<Db>,
    pub hosts: RwLock<BTreeMap<String, HostSnapshot>>,
    pub failures: Mutex<HashMap<String, u32>>,
    pub services: Mutex<BTreeMap<String, ServiceStatus>>,
    held: Mutex<(i64, Vec<(Alert, bool)>)>,
    pub peer: Mutex<PeerState>,
    wake: Mutex<Option<Sender<()>>>,
    queue: Mutex<Option<Sender<Message>>>,
    pending: Mutex<HashSet<String>>,
    logins: Mutex<(i64, u32)>,
    pub changes: tokio::sync::watch::Sender<u64>,
    links: Mutex<BTreeMap<String, u32>>,
    pub argon: tokio::sync::Semaphore,
    pub started: i64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl App {
    pub fn new(config: Config, db: Db) -> Self {
        let hosts = db::hosts(&db).into_iter().map(|s| (s.report.host.id.clone(), s)).collect();
        let cursor = db::meta(&db, "peer_cursor");
        let failures = db::alerts(&db)
            .into_iter()
            .filter(|a| a.rule == "check" && a.state == AlertState::Firing)
            .map(|a| (format!("{}|{}", a.host, a.item), rules::CHECK_FAILURES))
            .collect();
        let config = Arc::new(config);
        Self {
            boot: config.clone(),
            config: RwLock::new(config),
            config_write: Mutex::new(()),
            config_stamp: Mutex::new(None),
            config_path: None,
            db: Mutex::new(db),
            hosts: RwLock::new(hosts),
            failures: Mutex::new(failures),
            services: Mutex::new(BTreeMap::new()),
            held: Mutex::new((0, Vec::new())),
            peer: Mutex::new(PeerState { cursor, ..Default::default() }),
            wake: Mutex::new(None),
            queue: Mutex::new(None),
            pending: Mutex::new(HashSet::new()),
            logins: Mutex::new((0, 0)),
            changes: tokio::sync::watch::Sender::new(0),
            links: Mutex::new(BTreeMap::new()),
            argon: tokio::sync::Semaphore::new(1),
            started: now_ms(),
        }
    }

    pub fn db(&self) -> MutexGuard<'_, Db> {
        lock(&self.db)
    }

    pub fn config(&self) -> Arc<Config> {
        self.config.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn apply_config(&self, next: Config, bump: bool) -> Result<bool, String> {
        self.edit_config(bump, |_| Ok(next))
    }

    pub fn edit_config(&self, bump: bool, change: impl FnOnce(&Config) -> Result<Config, String>) -> Result<bool, String> {
        let _writer = lock(&self.config_write);
        let current = self.config();
        let mut next = change(&current)?;
        next.normalize()?;
        next.validate()?;
        if bump && !next.shared.same(&current.shared) {
            next.shared.revision = now_ms().max(current.shared.revision + 1);
        }
        if next == *current {
            return Ok(false);
        }
        if let Some(path) = &self.config_path {
            next.save(path)?;
            *lock(&self.config_stamp) = modified(path);
        }
        let removed: Vec<String> = current.shared.agents.keys().filter(|id| !next.shared.agents.contains_key(*id)).cloned().collect();
        *self.config.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(next);
        self.forget(&removed);
        Ok(true)
    }

    pub fn forget(&self, hosts: &[String]) {
        if hosts.is_empty() {
            return;
        }
        self.hosts.write().unwrap_or_else(|p| p.into_inner()).retain(|id, _| !hosts.contains(id));
        lock(&self.failures).retain(|key, _| !hosts.iter().any(|host| key.starts_with(&format!("{host}|"))));
        let db = self.db();
        for host in hosts {
            let _ = db::delete_host(&db, host);
            let event = Event {
                id: random_id(12),
                time: now_ms(),
                severity: Severity::Ok,
                kind: "removed".to_string(),
                target: host.clone(),
                text: "Removed from the agents".to_string(),
                note: "history stays until retention".to_string(),
            };
            let _ = db::save_event(&db, &event);
        }
        log(&format!("forgot {}", hosts.join(", ")));
    }

    pub fn peer_state(&self) -> MutexGuard<'_, PeerState> {
        lock(&self.peer)
    }

    pub fn held(&self) -> MutexGuard<'_, (i64, Vec<(Alert, bool)>)> {
        lock(&self.held)
    }

    pub fn logins(&self) -> MutexGuard<'_, (i64, u32)> {
        lock(&self.logins)
    }

    pub fn snapshots(&self) -> Vec<HostSnapshot> {
        self.hosts.read().unwrap_or_else(|p| p.into_inner()).values().cloned().collect()
    }

    pub fn put_snapshot(&self, snapshot: HostSnapshot) {
        let mut hosts = self.hosts.write().unwrap_or_else(|p| p.into_inner());
        let newer = hosts.get(&snapshot.report.host.id).is_none_or(|current| snapshot.last_seen >= current.last_seen);
        if newer {
            hosts.insert(snapshot.report.host.id.clone(), snapshot);
        }
    }

    pub fn peer_down(&self, now: i64) -> bool {
        self.config().peer.is_some() && now - self.peer_state().last_sync.unwrap_or(self.started) > PEER_TIMEOUT
    }

    pub fn owns(&self, site: &str) -> bool {
        self.config().peer_name() != Some(site)
    }

    pub fn enqueue(&self, message: Message) {
        if !lock(&self.pending).insert(message.id.clone()) {
            return;
        }
        if let Some(sender) = lock(&self.queue).as_ref() {
            let _ = sender.send(message);
        }
    }

    pub fn settle(&self, id: &str) {
        lock(&self.pending).remove(id);
    }

    pub fn wake_sync(&self) {
        if let Some(sender) = lock(&self.wake).as_ref() {
            let _ = sender.send(());
        }
        self.changed();
    }

    pub fn changed(&self) {
        self.changes.send_modify(|count| *count = count.wrapping_add(1));
    }

    pub fn link(&self, host: &str, open: bool) {
        {
            let mut links = lock(&self.links);
            let count = links.entry(host.to_string()).or_default();
            *count = if open { *count + 1 } else { count.saturating_sub(1) };
            if *count == 0 {
                links.remove(host);
            }
        }
        self.changed();
    }

    pub fn links(&self) -> Vec<String> {
        lock(&self.links).keys().cloned().collect()
    }

    pub fn connected(&self, host: &str) -> bool {
        if lock(&self.links).contains_key(host) {
            return true;
        }
        let peer = self.peer_state();
        peer.streams > 0 && peer.links.iter().any(|h| h == host)
    }

    pub fn store_report(&self, mut report: Report, now: i64) {
        report.time = now;
        let host = report.host.id.clone();
        let (standalone, own): (Vec<CheckResult>, Vec<CheckResult>) = report.checks.drain(..).partition(|c| c.name.starts_with(SERVICE_PREFIX));
        report.checks = own;
        {
            let mut failures = lock(&self.failures);
            let mut services = lock(&self.services);
            for mut check in standalone {
                check.name = check.name.trim_start_matches(SERVICE_PREFIX).to_string();
                let key = format!("{SERVICES_HOST}|{}", check.name);
                if check.status == CheckStatus::Failed {
                    *failures.entry(key).or_default() += 1;
                } else {
                    failures.remove(&key);
                }
                services.insert(check.name.clone(), ServiceStatus { check, runner: host.clone(), last_seen: now });
            }
            for check in &report.checks {
                let key = format!("{host}|{}", check.name);
                if check.status == CheckStatus::Failed {
                    *failures.entry(key).or_default() += 1;
                } else {
                    failures.remove(&key);
                }
            }
        }
        let samples: Vec<Sample> = report
            .metrics
            .iter()
            .map(|m| match m.item.strip_prefix(SERVICE_PREFIX) {
                Some(name) if m.group == "check" => {
                    Sample { host: SERVICES_HOST.to_string(), key: metric_key("check", name, &m.field), time: now, value: m.value }
                }
                _ => Sample { host: host.clone(), key: m.key(), time: now, value: m.value },
            })
            .collect();
        report.metrics.retain(|m| !(m.group == "check" && m.item.starts_with(SERVICE_PREFIX)));
        let snapshot = HostSnapshot { report, last_seen: now };
        {
            let mut db = self.db();
            if let Err(error) = db::insert_samples(&mut db, &samples) {
                log(&format!("storing samples for {host} failed: {error}"));
            }
            let _ = db::save_host(&db, &snapshot);
        }
        self.put_snapshot(snapshot);
        self.changed();
    }
}

fn start_agent(config: &Path) {
    let Some(settings) = ["agent.toml", "agent.json"].into_iter().map(|name| config.with_file_name(name)).find(|path| path.is_file()) else {
        return;
    };
    let binary =
        std::env::current_exe().map(|exe| exe.with_file_name("argus-agent")).ok().filter(|path| path.is_file()).unwrap_or_else(|| PathBuf::from("argus-agent"));
    let spawn = move || Command::new(&binary).args(["run", "--config"]).arg(&settings).spawn();
    let mut child = match spawn() {
        Ok(child) => child,
        Err(error) => {
            log(&format!("cannot start the built-in agent: {error}"));
            return;
        }
    };
    log("started the built-in agent");
    thread::spawn(move || {
        loop {
            let status = child.wait().map_or_else(|error| error.to_string(), |status| status.to_string());
            log(&format!("the built-in agent stopped ({status}), starting it again in 30 s"));
            thread::sleep(Duration::from_secs(30));
            match spawn() {
                Ok(next) => child = next,
                Err(error) => {
                    log(&format!("cannot start the built-in agent: {error}"));
                    return;
                }
            }
        }
    });
}

#[cfg(unix)]
fn leave_root() -> Result<(), String> {
    const NOBODY: libc::uid_t = 65534;
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let left = unsafe { libc::setgroups(0, std::ptr::null()) == 0 && libc::setgid(NOBODY) == 0 && libc::setuid(NOBODY) == 0 };
    if left { Ok(()) } else { Err(format!("cannot leave root: {}, the container needs the SETUID and SETGID capabilities", std::io::Error::last_os_error())) }
}

#[cfg(not(unix))]
fn leave_root() -> Result<(), String> {
    Ok(())
}

pub fn log(message: &str) {
    eprintln!("argus-node: {message}");
}

fn config_path(args: &[String]) -> PathBuf {
    args.windows(2)
        .find(|pair| pair[0] == "--config")
        .map(|pair| pair[1].clone())
        .or_else(|| std::env::var("ARGUS_NODE_CONFIG").ok())
        .unwrap_or_else(|| "/data/argus.json".to_string())
        .into()
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn load_or_create(path: &Path, create: bool) -> Result<Config, String> {
    if path.exists() || !create {
        return Config::load(path);
    }
    let config = Config::default();
    config.save(path)?;
    log(&format!("created {} with defaults", path.display()));
    Ok(config)
}

fn watch(app: &Arc<App>) {
    let Some(path) = app.config_path.clone() else {
        return;
    };
    let app = app.clone();
    *lock(&app.config_stamp) = modified(&path);
    every(Duration::from_secs(3), move || {
        let stamp = modified(&path);
        if stamp.is_none() || stamp == *lock(&app.config_stamp) {
            return;
        }
        *lock(&app.config_stamp) = stamp;
        match Config::load(&path).and_then(|next| app.apply_config(next, true)) {
            Ok(true) => log(&format!("reloaded {}", path.display())),
            Ok(false) => {}
            Err(error) => log(&format!("ignoring the changed config, the running one stays: {error}")),
        }
    });
}

fn every(period: Duration, task: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        loop {
            task();
            thread::sleep(period);
        }
    });
}

fn print_token() {
    let token = random_id(40);
    println!("token:  {token}");
    println!("sha256: {}", sha256_hex(token.as_bytes()));
}

pub fn router(app: Arc<App>) -> Router {
    let protected = Router::new()
        .route("/api/v1/overview", get(api::overview))
        .route("/api/v1/series", get(api::series))
        .route("/api/v1/events", get(api::events))
        .route("/api/v1/events/clear", post(api::clear_history))
        .route("/api/v1/rules", get(api::rules))
        .route("/api/v1/config", get(api::get_config))
        .route("/api/v1/config", put(api::put_config))
        .route("/api/v1/token", get(api::token))
        .route("/api/v1/notify-test", post(notify::test))
        .route("/api/v1/push/devices", post(notify::subscribe))
        .route("/api/v1/order", post(api::order))
        .route("/api/v1/password", post(session::change))
        .route_layer(axum::middleware::from_fn_with_state(app.clone(), session::require));
    let dashboard = Router::new()
        .merge(protected)
        .route("/api/v1/session", get(session::status))
        .route("/api/v1/login", post(session::login))
        .route("/api/v1/setup", post(session::setup))
        .route("/api/v1/logout", post(session::logout))
        .route_layer(axum::middleware::from_fn_with_state(app.clone(), session::enabled));
    Router::new()
        .route("/api/v1/health", get(|| async { "ok" }))
        .route("/api/v1/ingest", post(api::ingest).get(api::ingest_stream))
        .route("/api/v1/sync", post(sync::serve).get(sync::serve_stream))
        .route("/api/v1/hooks/zerobyte/{token}", post(zerobyte::webhook))
        .merge(dashboard)
        .route("/api/v1/push/receipt/{id}", post(notify::receipt))
        .fallback(ui::serve)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(app)
}

fn start_background(app: &Arc<App>, wake: Receiver<()>) {
    let evaluator = app.clone();
    every(Duration::from_secs(10), move || {
        let now = now_ms();
        rules::evaluate(&evaluator, now);
        notify::status(&evaluator, now);
    });
    let maintainer = app.clone();
    every(Duration::from_millis(5 * MINUTE as u64), move || {
        let now = now_ms();
        let retention = maintainer.config().shared.retention.clone();
        let mut db = maintainer.db();
        if let Err(error) = db::compact(&mut db, now, &retention) {
            log(&format!("compaction failed: {error}"));
        }
        db::prune(&db, now);
        drop(db);
        notify::resend(&maintainer, now);
    });
    let syncer = app.clone();
    thread::spawn(move || sync::run(&syncer, &wake));
    let poller = app.clone();
    thread::spawn(move || zerobyte::poll(&poller));
    watch(app);
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("token") => return print_token(),
        Some("serve" | "check" | "reset-password" | "--config") | None => {}
        Some(other) => {
            eprintln!("unknown command {other}, use serve, check, token or reset-password");
            std::process::exit(2);
        }
    }
    let path = config_path(&args);
    let command = args.get(1).map(String::as_str);
    let checking = command == Some("check");
    let resetting = command == Some("reset-password");
    let fail = |error: String| -> ! {
        eprintln!("{error}");
        std::process::exit(1);
    };
    if !checking && !resetting {
        start_agent(&path);
    }
    leave_root().unwrap_or_else(|error| fail(error));
    let mut config = load_or_create(&path, !checking && !resetting).unwrap_or_else(|error| fail(error));
    let hashed = config.clone();
    config.normalize().and_then(|_| config.validate()).unwrap_or_else(|error| fail(format!("invalid config {}: {error}", path.display())));
    if checking {
        println!("{} is valid: node {}, {} agents, {} rules", path.display(), config.name, config.shared.agents.len(), config.shared.rules.len());
        return;
    }
    if config != hashed {
        config.save(&path).unwrap_or_else(|error| fail(error));
        log("updated the config file with hashed secrets and generated keys");
    }
    let database = path.parent().unwrap_or(Path::new(".")).join(&config.database);
    let db = db::open(&database.to_string_lossy()).unwrap_or_else(|error| fail(format!("cannot open database {}: {error}", database.display())));
    if resetting && config.peer.as_ref().is_some_and(|peer| peer.url.is_none()) {
        fail("this node takes its password from the internal node, run reset-password there".to_string());
    }
    if resetting {
        session::reset(&db, now_ms()).unwrap_or_else(|error| fail(format!("cannot reset the password: {error}")));
        println!("dashboard password removed, open the dashboard within 15 minutes to set a new one");
        return;
    }
    if config.shared.agents.is_empty() {
        log("no agents configured, every report will be rejected");
    }
    if config.shared.push.as_ref().is_none_or(|p| p.devices.is_empty()) {
        log("no phone is set up for notifications, alerts only show in the dashboard");
    }
    if config.ui && !session::has_password(&db) {
        let _ = session::open_setup(&db, now_ms());
        log("no dashboard password yet, open the dashboard within 15 minutes to set it");
    }
    let listen = config.listen.clone();
    let mut app = App::new(config, db);
    app.config_path = Some(path);
    let app = Arc::new(app);
    let unknown: Vec<String> = app.snapshots().into_iter().map(|s| s.report.host.id).filter(|id| !app.config().shared.agents.contains_key(id)).collect();
    app.forget(&unknown);
    let (sender, receiver) = channel();
    *lock(&app.wake) = Some(sender);
    let (pusher, queue) = channel();
    *lock(&app.queue) = Some(pusher);
    let worker = app.clone();
    thread::spawn(move || notify::worker(&worker, &queue));
    start_background(&app, receiver);
    let listener = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|error| fail(format!("cannot listen on {listen}: {error}")));
    log(&format!("{} node listening on {listen}", app.config().name));
    let shutdown = async {
        #[cfg(unix)]
        {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(error) = axum::serve(listener, router(app)).with_graceful_shutdown(shutdown).await {
        eprintln!("server error: {error}");
    }
}

pub fn service_runner(app: &App, service: &config::Service, now: i64) -> Option<String> {
    if let Some(runner) = &service.runner {
        return Some(runner.clone());
    }
    let config = app.config();
    let snapshots = app.snapshots();
    let mut candidates: Vec<&HostSnapshot> = snapshots
        .iter()
        .filter(|s| s.report.host.site == config.home() && now - s.last_seen < rules::STALE && config.shared.agents.contains_key(&s.report.host.id))
        .collect();
    candidates.sort_by_key(|s| (config.shared.order.iter().position(|id| *id == s.report.host.id).unwrap_or(usize::MAX), s.report.host.id.clone()));
    candidates.first().map(|s| s.report.host.id.clone())
}
