use std::time::Duration;

use argus_model::{Alert, AlertState, Event, HostSummary, Overview, metric_key};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::route::Route;
use crate::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
    Hour,
    SixHours,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl Range {
    pub const OPTIONS: &'static [(Range, &'static str)] = &[
        (Range::Hour, "1h"),
        (Range::SixHours, "6h"),
        (Range::Day, "24h"),
        (Range::Week, "7d"),
        (Range::Month, "30d"),
        (Range::Quarter, "90d"),
        (Range::Year, "1y"),
    ];

    pub fn seconds(self) -> i64 {
        match self {
            Self::Hour => 3_600,
            Self::SixHours => 6 * 3_600,
            Self::Day => 86_400,
            Self::Week => 7 * 86_400,
            Self::Month => 30 * 86_400,
            Self::Quarter => 90 * 86_400,
            Self::Year => 365 * 86_400,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckFilter {
    All,
    Problems,
}

impl CheckFilter {
    pub const OPTIONS: &'static [(CheckFilter, &'static str)] = &[(CheckFilter::All, "All"), (CheckFilter::Problems, "Problems")];
}

#[derive(Clone, Copy)]
pub struct Store {
    pub clock: RwSignal<f64>,
    pub route: RwSignal<Route>,
    pub range: RwSignal<Range>,
    pub check_filter: RwSignal<CheckFilter>,
    pub overview: RwSignal<Option<Overview>>,
    pub offline: RwSignal<bool>,
    pub locked: RwSignal<bool>,
    pub gate: RwSignal<String>,
    pub events: RwSignal<Vec<Event>>,
    pub toast: RwSignal<Option<(u64, String)>>,
    pub theme: RwSignal<Theme>,
    pub config: RwSignal<Option<serde_json::Value>>,
    pub saved: RwSignal<Option<ConfigView>>,
    pub saving: RwSignal<bool>,
    pub save_error: RwSignal<Option<String>>,
    saves: StoredValue<u64>,
    pub dragging: RwSignal<Option<String>>,
    toasts: StoredValue<u64>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct ConfigView {
    pub config: serde_json::Value,
    pub version: String,
    pub path: Option<String>,
    pub restart: bool,
}

impl Store {
    pub fn new() -> Self {
        Self {
            clock: RwSignal::new(js_sys::Date::now()),
            route: RwSignal::new(Route::current()),
            range: RwSignal::new(Range::SixHours),
            check_filter: RwSignal::new(CheckFilter::All),
            overview: RwSignal::new(None),
            offline: RwSignal::new(false),
            locked: RwSignal::new(false),
            gate: RwSignal::new("login".to_string()),
            events: RwSignal::new(Vec::new()),
            toast: RwSignal::new(None),
            theme: RwSignal::new(Theme::load()),
            config: RwSignal::new(None),
            saved: RwSignal::new(None),
            saving: RwSignal::new(false),
            save_error: RwSignal::new(None),
            saves: StoredValue::new(0),
            dragging: RwSignal::new(None),
            toasts: StoredValue::new(0),
        }
    }

    pub fn refresh(&self) {
        let store = *self;
        spawn_local(async move {
            match api::get::<Overview>("/api/v1/overview").await {
                Ok(overview) => {
                    store.overview.set(Some(overview));
                    store.offline.set(false);
                    store.locked.set(false);
                }
                Err(failure) if failure.status == 401 => {
                    if !store.locked.get_untracked() {
                        store.check_session();
                    }
                    store.locked.set(true);
                    store.overview.set(None);
                }
                Err(_) => store.offline.set(true),
            }
        });
    }

    pub fn check_session(&self) {
        let store = *self;
        spawn_local(async move {
            if let Ok(session) = api::get::<serde_json::Value>("/api/v1/session").await {
                let state = session["state"].as_str().unwrap_or("login").to_string();
                if store.gate.with_untracked(|gate| *gate != state) {
                    store.gate.set(state);
                }
            }
        });
    }

    pub fn logout(&self) {
        let store = *self;
        spawn_local(async move {
            let _ = api::send("/api/v1/logout", &serde_json::json!({})).await;
            store.refresh();
        });
    }

    pub fn load_config(&self) {
        let store = *self;
        spawn_local(async move {
            match api::get::<ConfigView>("/api/v1/config").await {
                Ok(view) => {
                    store.config.set(Some(view.config.clone()));
                    store.saved.set(Some(view));
                }
                Err(failure) if failure.status == 401 => store.locked.set(true),
                Err(failure) => store.notify(format!("Loading the settings failed, {failure}")),
            }
        });
    }

    pub fn dirty(&self) -> bool {
        let saved = self.saved.with(|s| s.as_ref().map(|s| s.config.clone()));
        self.config.with(|c| c.is_some() && *c != saved)
    }

    pub fn schedule_save(&self) {
        let store = *self;
        let ticket = self.saves.get_value() + 1;
        self.saves.set_value(ticket);
        set_timeout(
            move || {
                if store.saves.get_value() != ticket || !store.dirty() {
                    return;
                }
                if store.saving.get_untracked() || typing() {
                    store.schedule_save();
                } else {
                    store.save_config();
                }
            },
            Duration::from_millis(700),
        );
    }

    fn save_config(&self) {
        let store = *self;
        let (Some(config), Some(version)) = (self.config.get_untracked(), self.saved.with_untracked(|s| s.as_ref().map(|s| s.version.clone()))) else {
            return;
        };
        self.saving.set(true);
        spawn_local(async move {
            match api::put::<_, ConfigView>("/api/v1/config", &serde_json::json!({ "version": version, "config": config.clone() })).await {
                Ok(view) => {
                    if view.restart {
                        store.notify("Saved, restart the node to use the new address or database");
                    }
                    if store.config.with_untracked(|c| c.as_ref() == Some(&config)) {
                        store.config.set(Some(view.config.clone()));
                    }
                    store.saved.set(Some(view));
                    store.save_error.set(None);
                    store.refresh();
                }
                Err(failure) if failure.status == 401 => store.locked.set(true),
                Err(failure) if failure.status == 409 => {
                    store.notify("The settings changed elsewhere and were reloaded");
                    store.save_error.set(None);
                    store.load_config();
                }
                Err(failure) => store.save_error.set(Some(failure.to_string())),
            }
            store.saving.set(false);
        });
    }

    pub fn refresh_events(&self) {
        let store = *self;
        spawn_local(async move {
            if let Ok(events) = api::get::<Vec<Event>>("/api/v1/events?limit=100").await {
                store.events.set(events);
            }
        });
    }

    pub fn loaded(&self) -> bool {
        self.overview.with(Option::is_some)
    }

    pub fn hosts(&self) -> Vec<HostSummary> {
        self.overview.with(|o| o.as_ref().map(|o| o.hosts.clone()).unwrap_or_default())
    }

    pub fn host_ids(&self) -> Vec<String> {
        self.overview.with(|o| o.as_ref().map(|o| o.hosts.iter().map(|h| h.info.id.clone()).collect()).unwrap_or_default())
    }

    pub fn move_host(&self, id: &str, to: usize) {
        let mut ids = self.host_ids();
        let Some(from) = ids.iter().position(|h| h == id) else { return };
        let moved = ids.remove(from);
        ids.insert(to.min(ids.len()), moved);
        self.overview.update(|o| {
            if let Some(o) = o {
                o.hosts.sort_by_key(|h| ids.iter().position(|id| *id == h.info.id));
            }
        });
        let store = *self;
        spawn_local(async move {
            if let Err(failure) = api::send("/api/v1/order", &serde_json::json!({ "hosts": ids })).await {
                store.notify(format!("Order not saved, {failure}"));
            }
            store.refresh();
        });
    }

    pub fn host(&self, id: &str) -> Option<HostSummary> {
        self.overview.with(|o| o.as_ref().and_then(|o| o.hosts.iter().find(|h| h.info.id == id).cloned()))
    }

    pub fn alerts(&self) -> Vec<Alert> {
        let mut list: Vec<Alert> =
            self.overview.with(|o| o.as_ref().map(|o| o.alerts.iter().filter(|a| a.state == AlertState::Firing).cloned().collect()).unwrap_or_default());
        list.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.since.cmp(&b.since)));
        list
    }

    pub fn active_count(&self) -> usize {
        self.alerts().len()
    }

    pub fn notify(&self, message: impl Into<String>) {
        let id = self.toasts.get_value() + 1;
        self.toasts.set_value(id);
        self.toast.set(Some((id, message.into())));
        let toast = self.toast;
        set_timeout(
            move || {
                if toast.try_with_untracked(|current| current.as_ref().is_some_and(|(shown, _)| *shown == id)).unwrap_or(false) {
                    toast.set(None);
                }
            },
            Duration::from_millis(3_200),
        );
    }
}

pub fn latest(host: &HostSummary, group: &str, item: &str, field: &str) -> Option<f64> {
    host.latest.iter().find(|m| m.group == group && m.item == item && m.field == field).map(|m| m.value)
}

pub fn limit(host: &HostSummary, group: &str, item: &str, field: &str) -> Option<f64> {
    host.limits.iter().find(|l| l.group == group && l.item == item && l.field == field).map(|l| l.above)
}

pub fn fullest(host: &HostSummary) -> Option<(String, f64)> {
    host.latest.iter().filter(|m| m.group == "fs" && m.field == "used").max_by(|a, b| a.value.total_cmp(&b.value)).map(|m| (m.item.clone(), m.value))
}

pub fn is_temperature(metric: &argus_model::Metric) -> bool {
    (metric.group == "temp" && metric.field == "value") || (metric.group == "smart" && metric.field == "temp")
}

pub fn temperatures(host: &HostSummary) -> Vec<&argus_model::Metric> {
    let mut list: Vec<&argus_model::Metric> = host.latest.iter().filter(|m| is_temperature(m)).collect();
    let hwmon_nvme = list.iter().any(|m| m.group == "temp" && m.item.starts_with("NVMe"));
    list.retain(|m| m.group == "temp" || !hwmon_nvme);
    list
}

pub fn hottest(host: &HostSummary) -> Option<(String, f64, f64)> {
    temperatures(host)
        .into_iter()
        .map(|m| (m.item.clone(), m.value, limit(host, &m.group, &m.item, &m.field).unwrap_or(85.0)))
        .max_by(|a, b| (a.1 / a.2).total_cmp(&(b.1 / b.2)))
}

pub fn key(group: &str, item: &str, field: &str) -> String {
    metric_key(group, item, field)
}

pub fn use_store() -> Store {
    expect_context::<Store>()
}

fn typing() -> bool {
    document().active_element().is_some_and(|element| match element.tag_name().as_str() {
        "TEXTAREA" => true,
        "INPUT" => !matches!(element.get_attribute("type").as_deref(), Some("checkbox" | "radio" | "button" | "submit" | "range")),
        _ => false,
    })
}
