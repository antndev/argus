use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

use crate::api;
use crate::route::Route;
use crate::store::{Store, use_store};

const KEYS: [(&str, &str); 10] = [
    ("1", "cpu"),
    ("2", "memory"),
    ("3", "disk"),
    ("4", "temperature"),
    ("5", "check"),
    ("6", "raid"),
    ("7", "reboot"),
    ("8", "offline"),
    ("9", "backup"),
    ("n", "peer"),
];

fn target(store: Store) -> Option<String> {
    match store.route.get_untracked() {
        Route::Host(id) => Some(id),
        _ => store.host_ids().into_iter().min(),
    }
}

fn label(state: &Value, fault: &str) -> String {
    ["host_faults", "node_faults"]
        .iter()
        .flat_map(|group| state[group].as_array().cloned().unwrap_or_default())
        .find(|f| f["id"] == fault)
        .and_then(|f| f["text"].as_str().map(str::to_string))
        .unwrap_or_else(|| fault.to_string())
}

fn active(state: &Value, host: &str, fault: &str) -> bool {
    state["faults"][host].as_array().is_some_and(|list| list.iter().any(|f| f == fault))
}

pub fn load(store: Store) {
    spawn_local(async move {
        store.simulation.set(api::get::<Value>("/api/v1/simulate").await.ok());
    });
}

fn send(store: Store, host: String, fault: &'static str) {
    spawn_local(async move {
        match api::post::<_, Value>("/api/v1/simulate", &json!({ "host": host, "fault": fault })).await {
            Ok(state) => {
                let message = match fault {
                    "clear" => "Simulation cleared, real values return with the next reports".to_string(),
                    "backup" | "peer" => format!("{} {}", label(&state, fault), if active(&state, "", fault) { "on" } else { "off" }),
                    _ => format!("{} on {host} {}", label(&state, fault), if active(&state, &host, fault) { "on" } else { "off" }),
                };
                store.notify(message);
                store.simulation.set(Some(state));
                store.refresh();
            }
            Err(failure) => store.notify(format!("Simulation failed, {failure}")),
        }
    });
}

pub fn install(store: Store) {
    load(store);
    let _ = window_event_listener(leptos::ev::keydown, move |event| {
        if store.simulation.with_untracked(Option::is_none) || event.ctrl_key() || event.meta_key() || event.alt_key() {
            return;
        }
        let typing = event
            .target()
            .and_then(|t| wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok())
            .is_some_and(|e| matches!(e.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"));
        if typing {
            return;
        }
        let key = event.key().to_lowercase();
        if key == "?" {
            store.simulation_help.update(|open| *open = !*open);
            return;
        }
        if key == "escape" {
            store.simulation_help.set(false);
            return;
        }
        if key == "0" {
            send(store, String::new(), "clear");
            return;
        }
        let Some(&(_, fault)) = KEYS.iter().find(|(k, _)| *k == key) else { return };
        let host = if matches!(fault, "backup" | "peer") { String::new() } else { target(store).unwrap_or_default() };
        if host.is_empty() && !matches!(fault, "backup" | "peer") {
            store.notify("No host to simulate on yet");
            return;
        }
        event.prevent_default();
        send(store, host, fault);
    });
}

#[component]
pub fn SimulationBadge() -> impl IntoView {
    let store = use_store();
    let count = move || {
        store.simulation.with(|s| s.as_ref().map_or(0, |s| s["faults"].as_object().map_or(0, |m| m.values().filter_map(Value::as_array).map(Vec::len).sum())))
    };
    let busy = move || count() != 0;
    view! {
        <Show when=move || store.simulation.with(Option::is_some)>
            <button class="sim-badge" class:on=busy type="button" title="Keyboard shortcuts for simulated problems" on:click=move |_| store.simulation_help.update(|o| *o = !*o)>
                {move || match count() {
                    0 => "Simulation, press ?".to_string(),
                    1 => "1 simulated problem".to_string(),
                    n => format!("{n} simulated problems"),
                }}
            </button>
        </Show>
    }
}

#[component]
pub fn SimulationPanel() -> impl IntoView {
    let store = use_store();
    let host = move || {
        store.route.track();
        store.overview.track();
        target(store).unwrap_or_else(|| "no host yet".to_string())
    };
    let rows = move || {
        let state = store.simulation.get().unwrap_or_default();
        let host = host();
        KEYS.iter()
            .map(|(key, fault)| {
                let on = if matches!(*fault, "backup" | "peer") { active(&state, "", fault) } else { active(&state, &host, fault) };
                view! {
                    <li class:on=on>
                        <kbd>{key.to_uppercase()}</kbd>
                        <span>{label(&state, fault)}</span>
                        <span class="sim-state">{if on { "on" } else { "" }}</span>
                    </li>
                }
            })
            .collect_view()
    };
    view! {
        <Show when=move || store.simulation_help.get() && store.simulation.with(Option::is_some)>
            <aside class="sim-panel" aria-label="Simulated problems">
                <div class="sim-head">
                    <b>"Simulate problems"</b>
                    <button class="btn small ghost" type="button" on:click=move |_| store.simulation_help.set(false)>"Close"</button>
                </div>
                <p>{move || format!("Host keys act on {}. Open a host to pick another one. Alerts follow the real rules, so a failing check needs three reports and an unreachable host 90 s.", host())}</p>
                <ul>{rows}</ul>
                <div class="sim-foot"><kbd>"0"</kbd>" Clear everything "<kbd>"?"</kbd>" Show or hide"</div>
            </aside>
        </Show>
    }
}
