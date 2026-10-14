use std::time::Duration;

use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;

use crate::api;
use crate::components::Toast;
use crate::format;
use crate::icons;
use crate::pages::{AlertsPage, ChecksPage, HostsPage, OverviewPage, SettingsPage};
use crate::route::Route;
use crate::store::{Store, use_store};

#[component]
pub fn App() -> impl IntoView {
    let store = Store::new();
    provide_context(store);
    store.refresh();
    Effect::new(move |_| store.theme.get().apply());
    Effect::new(move |_| {
        if store.dirty() {
            store.schedule_save();
        } else if store.save_error.with_untracked(Option::is_some) {
            store.save_error.set(None);
        }
    });
    set_interval(move || store.clock.set(js_sys::Date::now()), Duration::from_secs(1));
    set_interval(move || store.refresh(), Duration::from_secs(5));
    let _ = window_event_listener(leptos::ev::hashchange, move |_| store.route.set(Route::current()));

    let section = Memo::new(move |_| store.route.with(|r| r.section()));
    let page = move || match section.get() {
        "hosts" => view! { <HostsPage/> }.into_any(),
        "checks" => view! { <div class="body"><div class="page"><ChecksPage/></div></div> }.into_any(),
        "alerts" => view! { <div class="body"><div class="page"><AlertsPage/></div></div> }.into_any(),
        "settings" => view! { <SettingsPage/> }.into_any(),
        _ => view! { <div class="body"><div class="page"><OverviewPage/></div></div> }.into_any(),
    };

    store.check_session();

    view! {
        <Show when=move || !store.locked.get() fallback=|| view! { <Login/> }>
            <div class="app">
                <Sidebar/>
                <main class="main">
                    <Show when=move || store.offline.get()>
                        <div class="offline" role="alert">"Cannot reach the node. Showing the last known state."</div>
                    </Show>
                    {page}
                </main>
            </div>
        </Show>
        <Toast/>
    }
}

#[component]
fn Login() -> impl IntoView {
    let store = use_store();
    let password = RwSignal::new(String::new());
    let repeat = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let setup = move || store.gate.get() == "setup";
    let submit = move |event: SubmitEvent| {
        event.prevent_default();
        let secret = password.get_untracked();
        if setup() && secret != repeat.get_untracked() {
            error.set("The passwords do not match".to_string());
            return;
        }
        busy.set(true);
        let path = if setup() { "/api/v1/setup" } else { "/api/v1/login" };
        spawn_local(async move {
            match api::send(path, &serde_json::json!({ "password": secret })).await {
                Ok(()) => {
                    let _ = password.try_set(String::new());
                    let _ = repeat.try_set(String::new());
                    let _ = error.try_set(String::new());
                    store.refresh();
                }
                Err(failure) => {
                    let _ = error.try_set(if failure.status == 401 { "Wrong password".to_string() } else { format::capitalize(&failure.text) });
                    store.check_session();
                }
            }
            let _ = busy.try_set(false);
        });
    };
    let field = move |label: &'static str, value: RwSignal<String>, autocomplete: &'static str| {
        view! {
            <label class="field">
                <span>{label}</span>
                <input
                    class="input"
                    type="password"
                    autocomplete=autocomplete
                    required
                    prop:value=move || value.get()
                    on:input=move |ev| {
                        value.set(event_target_value(&ev));
                        error.set(String::new());
                    }
                />
            </label>
        }
    };
    let form = move || {
        match store.gate.get().as_str() {
        "pending" => view! {
            <div>
                <h1>"Waiting for the internal node"</h1>
                <p>"This node takes its password from the internal node with the next sync. Set the password there, then check again."</p>
            </div>
            <button class="btn primary" type="button" on:click=move |_| store.check_session()>"Check again"</button>
        }
        .into_any(),
        "expired" => view! {
            <div>
                <h1>"Setup time is over"</h1>
                <p>"A new password can only be set within 15 minutes after the node starts. Restart the node, or run argus-node reset-password on the server, then reload this page."</p>
            </div>
            <button class="btn primary" type="button" on:click=move |_| store.check_session()>"Check again"</button>
        }
        .into_any(),
        "setup" => view! {
            <div>
                <h1>"Set up Argus"</h1>
                <p>"Choose the password for this dashboard. It needs at least 10 characters."</p>
            </div>
            {field("New password", password, "new-password")}
            {field("Repeat password", repeat, "new-password")}
            <p class="login-error" role="alert">{move || error.get()}</p>
            <button class="btn primary" type="submit" disabled=move || busy.get()>{move || if busy.get() { "Saving" } else { "Create password" }}</button>
        }
        .into_any(),
        _ => view! {
            <div>
                <h1>"Sign in"</h1>
                <p>"Enter the dashboard password to continue."</p>
            </div>
            {field("Password", password, "current-password")}
            <p class="login-error" role="alert">{move || error.get()}</p>
            <button class="btn primary" type="submit" disabled=move || busy.get()>{move || if busy.get() { "Signing in" } else { "Sign in" }}</button>
        }
        .into_any(),
    }
    };
    view! {
        <div class="login">
            <form class="login-card" on:submit=submit>
                <div class="login-brand">
                    <span class="logo" inner_html=icons::LOGO></span>
                    <span>"Argus"</span>
                </div>
                {form}
            </form>
        </div>
    }
}

fn nav_item(
    section: &'static str,
    label: &'static str,
    href: &'static str,
    icon: &'static str,
    badge: impl Fn() -> Option<(String, bool)> + Send + Sync + 'static,
) -> impl IntoView {
    let store = use_store();
    let current = move || (store.route.with(|r| r.section()) == section).then_some("page");
    view! {
        <a class="nav-item" class:bottom=section == "settings" href=href aria-current=current>
            <span class="ico" inner_html=icon></span>
            <span class="nav-label">{label}</span>
            {move || badge().map(|(text, warn)| view! { <span class="count" class:warn=warn>{text}</span> })}
        </a>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let store = use_store();
    let nav = NodeRef::<leptos::html::Nav>::new();
    let pill = RwSignal::new(None::<(i32, i32)>);
    let ready = RwSignal::new(false);
    Effect::new(move |_| {
        store.route.track();
        let measure = move || {
            let found = nav
                .get_untracked()
                .and_then(|nav| nav.query_selector("[aria-current=\"page\"]").ok().flatten())
                .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
                .map(|item| (item.offset_top(), item.offset_height()));
            pill.set(found);
            set_timeout(move || ready.set(true), Duration::from_millis(50));
        };
        request_animation_frame(measure);
    });
    let resize = window_event_listener(leptos::ev::resize, move |_| {
        if let Some(item) = nav
            .get_untracked()
            .and_then(|nav| nav.query_selector("[aria-current=\"page\"]").ok().flatten())
            .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
        {
            pill.set(Some((item.offset_top(), item.offset_height())));
        }
    });
    on_cleanup(move || resize.remove());
    let nodes = move || {
        store
            .overview
            .with(|o| o.as_ref().map(|o| o.nodes.clone()).unwrap_or_default())
            .into_iter()
            .filter(|_| store.overview.with(|o| o.as_ref().is_some_and(|o| o.nodes.len() > 1)))
            .map(|node| {
                let detail = if node.this {
                    "This node".to_string()
                } else {
                    match (node.reachable, node.last_sync) {
                        (true, Some(at)) => format!("Synced {}", format::ago(at as f64, store.clock.get_untracked())),
                        (false, Some(at)) => format!("Unreachable since {}", format::ago(at as f64, store.clock.get_untracked())),
                        (_, None) => "Never synced".to_string(),
                    }
                };
                let role = if node.internal { "Internal node" } else { "External node" };
                view! {
                    <div class="node" title=node.name.clone()>
                        <span class="dot" class:critical=!node.reachable></span>
                        <span class="node-name">{role}</span>
                        <span class="node-detail">{detail}</span>
                    </div>
                }
            })
            .collect_view()
    };
    let pill_style = move || match pill.get() {
        Some((top, height)) => format!("height:{height}px;transform:translateY({top}px);opacity:1"),
        None => "opacity:0".to_string(),
    };
    view! {
        <aside class="side" aria-label="Sidebar">
            <a class="brand" href="#/">
                <span class="logo" inner_html=icons::LOGO></span>
                <span>"Argus"</span>
            </a>
            <nav class="nav" node_ref=nav aria-label="Sections">
                <span class="nav-pill" class:ready=move || ready.get() style=pill_style aria-hidden="true"></span>
                {nav_item("overview", "Overview", "#/", icons::OVERVIEW, || None)}
                {nav_item("hosts", "Hosts", "#/hosts", icons::HOSTS, move || {
                    let down = store.hosts().iter().filter(|h| h.state >= argus_model::Severity::Warning).count();
                    (down > 0).then(|| (down.to_string(), true))
                })}
                {nav_item("checks", "Services", "#/services", icons::CHECKS, move || {
                    let failing = crate::components::all_checks(store).iter().filter(|row| !row.waiting && row.check.status != argus_model::CheckStatus::Ok).count();
                    (failing > 0).then(|| (failing.to_string(), true))
                })}
                {nav_item("alerts", "Alerts", "#/alerts", icons::ALERTS, move || {
                    let active = store.alerts().len();
                    (active > 0).then(|| (active.to_string(), true))
                })}
                <div class="nodes">{nodes}</div>
                {nav_item("settings", "Settings", "#/settings", icons::SETTINGS, move || store.save_error.with(Option::is_some).then(|| ("Not saved".to_string(), true)))}
            </nav>
        </aside>
    }
}
