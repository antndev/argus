use argus_model::{Alert, BackupSchedule, BackupState, CheckResult, CheckStatus, HostSummary, SERVICES_HOST, SeriesData, Severity, metric_key};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::format;
use crate::icons;
use crate::route::Route;
use crate::store::{Store, fullest, hottest, latest, use_store};

pub fn class(severity: Severity) -> &'static str {
    match severity {
        Severity::Ok => "ok",
        Severity::Muted => "muted",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

pub fn label(severity: Severity) -> &'static str {
    match severity {
        Severity::Ok => "OK",
        Severity::Muted => "Waiting",
        Severity::Warning => "Warning",
        Severity::Critical => "Critical",
    }
}

pub fn check_severity(status: CheckStatus) -> Severity {
    match status {
        CheckStatus::Ok => Severity::Ok,
        CheckStatus::Warning => Severity::Warning,
        CheckStatus::Failed => Severity::Critical,
    }
}

pub fn backup_status(state: BackupState) -> (Severity, &'static str) {
    match state {
        BackupState::Ok => (Severity::Ok, "OK"),
        BackupState::Running => (Severity::Muted, "Running"),
        BackupState::Warning => (Severity::Warning, "Warning"),
        BackupState::Failed => (Severity::Critical, "Failed"),
        BackupState::Unknown => (Severity::Muted, "No run yet"),
    }
}

#[component]
pub fn Pill(severity: Severity, #[prop(optional, into)] text: Option<String>) -> impl IntoView {
    let text = text.unwrap_or_else(|| label(severity).to_string());
    view! {
        <span class=format!("pill {}", class(severity))>
            <span class="ico" inner_html=icons::severity(severity)></span>
            {text}
        </span>
    }
}

pub fn sev_icon(severity: Severity) -> impl IntoView {
    view! { <span class=format!("sev {}", class(severity)) title=label(severity) inner_html=icons::severity(severity)></span> }
}

pub fn segmented<T>(value: RwSignal<T>, options: &'static [(T, &'static str)], label: &'static str) -> impl IntoView
where
    T: Copy + PartialEq + Send + Sync + 'static,
{
    let index = move || options.iter().position(|(option, _)| *option == value.get()).unwrap_or(0);
    let buttons = options
        .iter()
        .map(|&(option, text)| {
            view! {
                <button type="button" aria-pressed=move || (value.get() == option).to_string() on:click=move |_| value.set(option)>
                    {text}
                </button>
            }
        })
        .collect_view();
    view! {
        <div class="seg" role="group" aria-label=label style=format!("--n:{}", options.len())>
            <span class="seg-thumb" style=move || format!("--x:{}", index())></span>
            {buttons}
        </div>
    }
}

fn meter(
    label: impl Fn() -> String + Send + Sync + 'static,
    text: impl Fn() -> String + Send + Sync + 'static,
    fill: impl Fn() -> f64 + Send + Sync + 'static,
    warn: impl Fn() -> bool + Send + Sync + 'static,
) -> impl IntoView {
    view! {
        <div class="m">
            <dt>{label}</dt>
            <dd>
                <span class="mv">{text}</span>
                <span class=move || if warn() { "bar warning" } else { "bar" }>
                    <i style=move || format!("width:{:.1}%", fill().clamp(2.0, 100.0))></i>
                </span>
            </dd>
        </div>
    }
}

fn spark_shape(values: &[Option<f64>]) -> (String, String, f64) {
    const WIDTH: f64 = 200.0;
    const HEIGHT: f64 = 34.0;
    let points: Vec<(f64, f64)> = values.iter().enumerate().filter_map(|(i, v)| v.map(|v| (i, v))).map(|(i, v)| (i as f64, v)).collect();
    if points.len() < 2 {
        return (String::new(), String::new(), 100.0);
    }
    let last = (values.len() - 1).max(1) as f64;
    let max = points.iter().map(|p| p.1).fold(40.0, f64::max) * 1.1;
    let scaled: Vec<(f64, f64)> = points.iter().map(|(i, v)| (i / last * WIDTH, HEIGHT - v / max * HEIGHT)).collect();
    let line: String = scaled.iter().enumerate().map(|(i, (x, y))| format!("{}{x:.1} {y:.1}", if i == 0 { "M" } else { "L" })).collect();
    let (first_x, last_x) = (scaled[0].0, scaled[scaled.len() - 1].0);
    let area = format!("{line}L{last_x:.1} {HEIGHT}L{first_x:.1} {HEIGHT}Z");
    (line, area, scaled[scaled.len() - 1].1 / HEIGHT * 100.0)
}

#[component]
pub fn Sparkline(host: String, index: usize) -> impl IntoView {
    let store = use_store();
    let values = RwSignal::new(Vec::<Option<f64>>::new());
    let minute = Memo::new(move |_| (store.clock.get() / 60_000.0).floor() as i64);
    let url = format!("/api/v1/series?host={}&keys={}&range=3600", api::encode(&host), api::encode(&metric_key("cpu", "", "total")));
    Effect::new(move |_| {
        minute.track();
        let url = url.clone();
        spawn_local(async move {
            if let Ok(data) = api::get::<SeriesData>(&url).await {
                let _ = values.try_set(data.values.into_iter().next().unwrap_or_default());
            }
        });
    });
    let shape = Memo::new(move |_| values.with(|v| spark_shape(v)));
    view! {
        <div class="hc-spark" title="CPU, last hour" style=format!("--i:{index}")>
            <svg viewBox="0 0 200 34" preserveAspectRatio="none" aria-hidden="true">
                <path class="a" d=move || shape.with(|s| s.1.clone())></path>
                <path class="l" d=move || shape.with(|s| s.0.clone()) vector-effect="non-scaling-stroke"></path>
            </svg>
            <Show when=move || shape.with(|s| !s.0.is_empty())>
                <span class="end" style=move || format!("top:{:.1}%", shape.with(|s| s.2))></span>
            </Show>
        </div>
    }
}

pub fn move_keys(store: Store, id: String) -> impl Fn(leptos::ev::KeyboardEvent) + 'static {
    move |ev| {
        if !ev.alt_key() {
            return;
        }
        let step: isize = match ev.key().as_str() {
            "ArrowLeft" | "ArrowUp" => -1,
            "ArrowRight" | "ArrowDown" => 1,
            _ => return,
        };
        ev.prevent_default();
        if let Some(at) = store.host_ids().iter().position(|h| *h == id) {
            store.move_host(&id, at.saturating_add_signed(step));
        }
    }
}

#[component]
pub fn HostCard(id: String, index: usize) -> impl IntoView {
    let store = use_store();
    let lookup = id.clone();
    let host = Memo::new(move |_| store.host(&lookup));
    let read = move |f: fn(&HostSummary) -> Option<f64>| host.with(|h| h.as_ref().and_then(f));
    let state = move || host.with(|h| h.as_ref().map(|h| h.state).unwrap_or_default());
    let waiting = move || host.with(|h| h.as_ref().is_some_and(|h| h.last_seen == 0));
    let meta = move || {
        host.with(|h| h.as_ref().map(|h| if h.last_seen == 0 { "Waiting for the first report".to_string() } else { format!("{}, {}", h.info.os, h.info.arch) }))
            .unwrap_or_default()
    };
    let seen = move || {
        host.with(|h| {
            h.as_ref().map(|h| {
                if h.last_seen == 0 {
                    "Start the agent with its token".to_string()
                } else {
                    format!("Last report {}", format::ago(h.last_seen as f64, store.clock.get()))
                }
            })
        })
        .unwrap_or_default()
    };
    let disk = move || host.with(|h| h.as_ref().and_then(fullest));
    let heat = move || host.with(|h| h.as_ref().and_then(hottest));
    let has_heat = move || heat().is_some();
    let href = Route::Host(id.clone()).href();
    let spark = StoredValue::new(id.clone());
    view! {
        <a
            class=move || format!("host-card {}", class(state()))
            class:pending=waiting
            href=href
            style=format!("--i:{index}")
        >
            <div class="hc-id">
                <div class="hc-top">
                    <span class="hc-name">{id.clone()}</span>
                    {move || view! { <Pill severity=state()/> }}
                </div>
                <div class="hc-meta">{meta}</div>
                <div class="hc-foot">{seen}</div>
            </div>
            <Show when=move || !waiting()>
            <Sparkline host=spark.get_value() index=index/>
            <dl class="hc-m">
                {meter(
                    || "CPU".to_string(),
                    move || read(|h| latest(h, "cpu", "", "total")).map(format::percent).unwrap_or_else(|| "n/a".into()),
                    move || read(|h| latest(h, "cpu", "", "total")).unwrap_or_default(),
                    move || read(|h| latest(h, "cpu", "", "total")).is_some_and(|v| v >= 85.0),
                )}
                {meter(
                    || "Memory".to_string(),
                    move || read(|h| latest(h, "memory", "", "used")).map(format::percent).unwrap_or_else(|| "n/a".into()),
                    move || read(|h| latest(h, "memory", "", "used")).unwrap_or_default(),
                    move || read(|h| latest(h, "memory", "", "used")).is_some_and(|v| v >= 85.0),
                )}
                {meter(
                    move || disk().map(|(mount, _)| format!("Disk {mount}")).unwrap_or_else(|| "Disk".into()),
                    move || disk().map(|(_, v)| format::percent(v)).unwrap_or_else(|| "n/a".into()),
                    move || disk().map(|(_, v)| v).unwrap_or_default(),
                    move || disk().is_some_and(|(_, v)| v >= 85.0),
                )}
                <Show
                    when=has_heat
                    fallback=|| view! { <div class="m"><dt>"Temperature"</dt><dd><span class="mv dim">"No sensors"</span></dd></div> }
                >
                    {meter(
                        || "Temperature".to_string(),
                        move || heat().map(|(_, v, _)| format::celsius(v)).unwrap_or_default(),
                        move || heat().map(|(_, v, limit)| v / limit * 100.0).unwrap_or_default(),
                        move || heat().is_some_and(|(_, v, limit)| v >= limit),
                    )}
                </Show>
            </dl>
            </Show>
        </a>
    }
}

#[component]
pub fn AlertList(#[prop(optional, into)] host: Option<String>) -> impl IntoView {
    let store = use_store();
    let only = host.clone();
    let rows = move || store.alerts().into_iter().filter(|alert| only.as_ref().is_none_or(|h| &alert.host == h)).collect::<Vec<_>>();
    let scoped = host.is_some();
    let clear = move || !scoped && store.loaded() && store.alerts().is_empty();
    view! {
        <div class="alert-list">
            <Show when=clear>
                <div class="all-clear">
                    <span class="ico" inner_html=icons::OK></span>
                    <span>"Nothing needs attention right now."</span>
                </div>
            </Show>
            <For each=rows key=|alert| (alert.key.clone(), alert.severity, alert.title.clone()) children=move |alert| alert_row(store, alert, !scoped)/>
        </div>
    }
}

fn alert_row(store: Store, alert: Alert, link: bool) -> impl IntoView {
    let host_link = Route::Host(alert.host.clone()).href();
    let since = alert.fired_at.unwrap_or(alert.since) as f64;
    let detail = alert.detail.clone();
    let full = alert.detail.clone();
    let age = move || format!("For {}", format::duration(store.clock.get() - since));
    view! {
        <div class=format!("alert-row {}", class(alert.severity))>
            {sev_icon(alert.severity)}
            <div class="ar-body">
                <div><span class="ar-host">{match alert.host.as_str() { "zerobyte" => "ZeroByte".to_string(), SERVICES_HOST => "Service".to_string(), host => host.to_string() }}</span>{alert.title.clone()}</div>
                {(!detail.is_empty()).then(|| view! { <div class="ar-detail" title=full>{detail}</div> })}
                <div class="ar-meta">{age}</div>
            </div>
            {(link && store.host(&alert.host).is_some()).then(|| view! { <div class="ar-act"><a class="btn ghost" href=host_link>"Open host"</a></div> })}
        </div>
    }
}

#[component]
pub fn Toast() -> impl IntoView {
    let store = use_store();
    move || store.toast.get().map(|(_, message)| view! { <div class="toast" role="status">{message}</div> })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServiceRow {
    pub host: String,
    pub runner: String,
    pub check: CheckResult,
    pub waiting: bool,
}

impl ServiceRow {
    pub fn standalone(&self) -> bool {
        self.host == SERVICES_HOST
    }

    pub fn severity(&self) -> Severity {
        if self.waiting { Severity::Muted } else { check_severity(self.check.status) }
    }
}

pub fn all_checks(store: Store) -> Vec<ServiceRow> {
    let standalone = store.overview.with(|o| o.as_ref().map(|o| o.services.clone()).unwrap_or_default()).into_iter().map(|s| ServiceRow {
        host: SERVICES_HOST.to_string(),
        runner: s.runner,
        waiting: s.last_seen == 0,
        check: s.check,
    });
    let hosted = store
        .hosts()
        .into_iter()
        .flat_map(|h| h.checks.into_iter().map(move |check| ServiceRow { host: h.info.id.clone(), runner: h.info.id.clone(), check, waiting: false }));
    standalone.chain(hosted).collect()
}

#[component]
pub fn ServiceList() -> impl IntoView {
    let store = use_store();
    let checks = Memo::new(move |_| all_checks(store));
    view! {
        <Show when=move || !checks.with(Vec::is_empty) fallback=move || store.loaded().then(|| view! { <p class="meta">"No services yet. Add them under Settings, Services."</p> })>
        <ul class="svc sheet">
            {move || {
                checks
                    .get()
                    .into_iter()
                    .map(|row| {
                        let severity = row.severity();
                        let check = row.check;
                        view! {
                            <li title=format!("{} on {}", check.message, row.runner)>
                                {sev_icon(severity)}
                                <span class="svc-name">{check.name.clone()}</span>
                                <span class="tag">{check.kind.clone()}</span>
                                <span class="svc-result">{check.latency_ms.map(format::latency).unwrap_or_default()}</span>
                            </li>
                        }
                    })
                    .collect_view()
            }}
        </ul>
        </Show>
    }
}

fn repository(backup: &BackupSchedule) -> impl IntoView + use<> {
    let kind = Some(backup.repository_kind.clone()).filter(|kind| !kind.is_empty() && !backup.repository.contains(kind.as_str()));
    view! {
        {backup.repository.clone()}
        {kind.map(|kind| view! { <span class="sub">{kind}</span> })}
    }
}

fn mirror(store: Store, name: String, state: BackupState, finished: Option<i64>, message: String) -> impl IntoView {
    let (severity, _) = backup_status(state);
    let title = message.clone();
    let detail = move || match (state, message.is_empty(), finished) {
        (BackupState::Unknown, _, _) => "Not synced yet".to_string(),
        (_, false, _) => message.clone(),
        (_, true, Some(at)) => format!("Synced {}", format::ago(at as f64, store.clock.get())),
        (_, true, None) => String::new(),
    };
    view! {
        <div class="mirror">
            <div>{sev_icon(severity)}{name}</div>
            <span class="sub clip" title=title>{detail}</span>
        </div>
    }
}

fn mirrors(store: Store, backup: &BackupSchedule) -> impl IntoView + use<> {
    if backup.mirrors.is_empty() && backup.mirror_state != BackupState::Unknown {
        return mirror(store, "Mirror".to_string(), backup.mirror_state, backup.mirror_finished, backup.mirror_message.clone()).into_any();
    }
    backup.mirrors.iter().map(|m| mirror(store, m.repository.clone(), m.state, m.finished, m.message.clone())).collect_view().into_any()
}

fn backup_row(store: Store, backup: BackupSchedule, columns: (bool, bool)) -> impl IntoView {
    let (severity, text) = backup_status(backup.state);
    let ago = move |at: Option<i64>| at.map(|t| format::ago(t as f64, store.clock.get())).unwrap_or_else(|| "never".to_string());
    let last = backup.last_success;
    let message = backup.message.clone();
    let added = (!backup.added.is_empty()).then(|| format!("{} added", backup.added));
    let name = backup.name.clone();
    let remove = move |_| {
        if !window().confirm_with_message(&format!("Remove the backup schedule {name}? It comes back with its next run.")).unwrap_or(false) {
            return;
        }
        let name = name.clone();
        spawn_local(async move {
            match api::send("/api/v1/backups/remove", &serde_json::json!({ "name": name })).await {
                Ok(()) => store.refresh(),
                Err(failure) => store.notify(format!("Not removed, {failure}")),
            }
        });
    };
    view! {
        <tr>
            <td><Pill severity=severity text=text/></td>
            <td><span class="check-name">{backup.name.clone()}</span>{(!message.is_empty()).then(|| view! { <span class="sub clip" title=message.clone()>{message.clone()}</span> })}</td>
            <td>{last.map(|t| format::hhmm(t as f64)).unwrap_or_default()}<span class="sub">{move || ago(last)}</span></td>
            <td class="hide-sm">{backup.size.clone()}{added.map(|text| view! { <span class="sub">{text}</span> })}</td>
            {columns.0.then(|| view! { <td>{repository(&backup)}</td> })}
            {columns.1.then(|| view! { <td>{mirrors(store, &backup)}</td> })}
            <td class="num">
                <button class="btn small icon danger" type="button" aria-label="Remove schedule" title="Remove schedule" on:click=remove>
                    <span class="ico" inner_html=icons::TRASH></span>
                </button>
            </td>
        </tr>
    }
}

#[component]
pub fn BackupTable() -> impl IntoView {
    let store = use_store();
    let backups = Memo::new(move |_| store.overview.with(|o| o.as_ref().map(|o| o.backups.clone()).unwrap_or_default()));
    let columns = Memo::new(move |_| {
        backups
            .with(|list| (list.iter().any(|b| !b.repository.is_empty()), list.iter().any(|b| !b.mirrors.is_empty() || b.mirror_state != BackupState::Unknown)))
    });
    let empty = move || backups.with(Vec::is_empty);
    view! {
        <Show
            when=move || !empty()
            fallback=|| view! { <p class="meta">"No backup data yet. Point ZeroByte's webhook at this node or set its API key."</p> }
        >
            <div class="tbl-wrap sheet stack">
                <table>
                    <thead>
                        <tr>
                            <th>"Status"</th>
                            <th>"Schedule"</th>
                            <th>"Last success"</th>
                            <th class="hide-sm">"Size"</th>
                            {move || columns.get().0.then(|| view! { <th>"Repository"</th> })}
                            {move || columns.get().1.then(|| view! { <th>"Mirror"</th> })}
                            <th></th>
                        </tr>
                    </thead>
                    <tbody>{move || backups.get().into_iter().map(|b| backup_row(store, b, columns.get())).collect_view()}</tbody>
                </table>
            </div>
        </Show>
    }
}

pub fn uptime_strip(history: Option<Vec<Option<f64>>>) -> impl IntoView {
    let Some(values) = history else {
        return view! { <span class="strip-wrap"><span class="dim">"Loading"</span></span> }.into_any();
    };
    let cells: Vec<Option<f64>> = (0..48)
        .map(|i| {
            let from = i * values.len() / 48;
            let to = ((i + 1) * values.len() / 48).max(from + 1).min(values.len());
            values.get(from..to).and_then(|slice| slice.iter().flatten().copied().reduce(f64::min))
        })
        .collect();
    let known: Vec<f64> = values.iter().flatten().copied().collect();
    let uptime = if known.is_empty() { "n/a".to_string() } else { format!("{:.1}%", known.iter().sum::<f64>() / known.len() as f64 * 100.0) };
    let cells = cells
        .into_iter()
        .map(|cell| {
            let state = match cell {
                None => "none",
                Some(v) if v >= 0.999 => "",
                Some(_) => "bad",
            };
            view! { <i class=state></i> }
        })
        .collect_view();
    view! {
        <span class="strip-wrap">
            <span class="strip">{cells}</span>
            <span class="dim">{uptime}</span>
        </span>
    }
    .into_any()
}
