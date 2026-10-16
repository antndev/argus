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
    let bad = RwSignal::new(false);
    let shake = RwSignal::new(0u32);
    let busy = RwSignal::new(false);
    let reveal = RwSignal::new(false);
    let fail = move |text: String| {
        let _ = error.try_set(text);
        let _ = bad.try_set(true);
        let _ = shake.try_update(|n| *n += 1);
    };
    let submit = move |event: SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let setup = store.gate.get_untracked() == "setup";
        let secret = password.get_untracked();
        if setup && secret.chars().count() < 10 {
            fail("Use at least 10 characters".to_string());
            return;
        }
        if setup && secret != repeat.get_untracked() {
            fail("The passwords do not match".to_string());
            return;
        }
        busy.set(true);
        let path = if setup { "/api/v1/setup" } else { "/api/v1/login" };
        spawn_local(async move {
            match api::send(path, &serde_json::json!({ "password": secret })).await {
                Ok(()) => {
                    let _ = password.try_set(String::new());
                    let _ = repeat.try_set(String::new());
                    let _ = error.try_set(String::new());
                    store.refresh();
                }
                Err(failure) => {
                    fail(if failure.status == 401 { String::new() } else { format::capitalize(&failure.text) });
                    store.check_session();
                }
            }
            let _ = busy.try_set(false);
        });
    };
    let field = move |value: RwSignal<String>, placeholder: &'static str, autocomplete: &'static str, eye: bool| {
        view! {
            <div class="login-field" class:bad=move || bad.get()>
                <input
                    class:masked=move || !reveal.get() && !value.get().is_empty()
                    type=move || if reveal.get() { "text" } else { "password" }
                    autocomplete=autocomplete
                    placeholder=placeholder
                    aria-label=placeholder
                    autofocus=eye
                    spellcheck="false"
                    prop:value=move || value.get()
                    on:input=move |ev| {
                        value.set(event_target_value(&ev));
                        error.set(String::new());
                        bad.set(false);
                    }
                />
                {eye.then(|| view! {
                    <button
                        class="login-eye"
                        type="button"
                        aria-label=move || if reveal.get() { "Hide password" } else { "Show password" }
                        on:click=move |_| reveal.update(|shown| *shown = !*shown)
                        inner_html=move || if reveal.get() { icons::EYE_OFF } else { icons::EYE }
                    ></button>
                })}
            </div>
        }
    };
    let fields = move || match shake.get() {
        0 => "login-fields",
        n if n % 2 == 1 => "login-fields shake-a",
        _ => "login-fields shake-b",
    };
    let note = move || view! { <p class="login-note" role="alert">{move || error.get()}</p> };
    let ready = move || !password.get().is_empty() && (store.gate.get() != "setup" || !repeat.get().is_empty());
    let submit_button = move |label: &'static str| {
        view! {
            <button class="btn primary" type="submit" aria-busy=move || busy.get().to_string() aria-label=label disabled=move || !ready() || busy.get()>
                {move || if busy.get() { view! { <span class="spinner" aria-hidden="true"></span> }.into_any() } else { label.into_any() }}
            </button>
        }
    };
    let form = move || match store.gate.get().as_str() {
        "pending" => view! {
            <h1>"Waiting for the internal node"</h1>
            <p>"This node takes its password from the internal node with the next sync."</p>
            <button class="btn primary" type="button" on:click=move |_| store.check_session()>"Check again"</button>
        }
        .into_any(),
        "expired" => view! {
            <h1>"Setup time is over"</h1>
            <p>"Restart the node or run argus-node reset-password on the server, then check again."</p>
            <button class="btn primary" type="button" on:click=move |_| store.check_session()>"Check again"</button>
        }
        .into_any(),
        "setup" => view! {
            <h1>"Set up Argus"</h1>
            <div class=fields>
                {field(password, "Password", "new-password", true)}
                {field(repeat, "Repeat password", "new-password", false)}
            </div>
            {submit_button("Create password")}
            {note()}
        }
        .into_any(),
        _ => view! {
            <h1>"Welcome!"</h1>
            <div class=fields>{field(password, "Password", "current-password", true)}</div>
            {submit_button("Sign in")}
            {note()}
        }
        .into_any(),
    };
    view! {
        <div class="login">
            <form class="login-card" on:submit=submit>
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
                    match (node.reachable, node.connected, node.last_sync) {
                        (true, true, _) => "Connected".to_string(),
                        (true, false, Some(at)) => format!("Synced {}", format::ago(at as f64, store.clock.get_untracked())),
                        (false, _, Some(at)) => format!("Unreachable since {}", format::ago(at as f64, store.clock.get_untracked())),
                        (_, _, None) => "Never synced".to_string(),
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
