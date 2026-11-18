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
    let code = RwSignal::new(String::new());
    let ticket = RwSignal::new(None::<String>);
    let until = RwSignal::new(0.0);
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
    let done = move || {
        for signal in [password, repeat, code, error] {
            let _ = signal.try_set(String::new());
        }
        let _ = ticket.try_set(None);
        store.refresh();
    };
    let back = move || {
        ticket.set(None);
        code.set(String::new());
        error.set(String::new());
        bad.set(false);
    };
    let handle = move |failure: api::Failure| match failure.status {
        429 => {
            let _ = until.try_set(js_sys::Date::now() + f64::from(failure.retry.max(1)) * 1_000.0);
        }
        401 => {
            let _ = code.try_set(String::new());
            fail(String::new());
        }
        403 if ticket.get_untracked().is_some() => {
            back();
            let _ = error.try_set(format::capitalize(&failure.text));
        }
        _ => {
            fail(format::capitalize(&failure.text));
            store.check_session();
        }
    };
    let submit = move |event: SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() || until.get_untracked() > js_sys::Date::now() {
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
        spawn_local(async move {
            if let Some(pending) = ticket.get_untracked() {
                let body = serde_json::json!({ "ticket": pending, "code": code.get_untracked() });
                match api::send("/api/v1/login/totp", &body).await {
                    Ok(()) => done(),
                    Err(failure) => handle(failure),
                }
            } else if setup {
                match api::send("/api/v1/setup", &serde_json::json!({ "password": secret })).await {
                    Ok(()) => done(),
                    Err(failure) => handle(failure),
                }
            } else {
                match api::post_reply::<_, serde_json::Value>("/api/v1/login", &serde_json::json!({ "password": secret })).await {
                    Ok(None) => done(),
                    Ok(Some(reply)) => {
                        let _ = password.try_set(String::new());
                        let _ = ticket.try_set(reply["ticket"].as_str().map(str::to_string));
                        request_animation_frame(focus_field);
                    }
                    Err(failure) => handle(failure),
                }
            }
            let _ = busy.try_set(false);
        });
    };
    let field = move |value: RwSignal<String>, placeholder: &'static str, autocomplete: &'static str, eye: bool| {
        let numeric = autocomplete == "one-time-code";
        view! {
            <div class="login-field" class:bad=move || bad.get()>
                <input
                    class:masked=move || !numeric && !reveal.get() && !value.get().is_empty()
                    type=move || if numeric || reveal.get() { "text" } else { "password" }
                    inputmode=numeric.then_some("numeric")
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
    let waiting = move || until.get() - store.clock.get();
    let note = move || {
        view! {
            <p class="login-note" role="alert">
                {move || match waiting() {
                    left if left > 0.0 => format!("Too many attempts, try again in {}", format::duration(left)),
                    _ => error.get(),
                }}
            </p>
        }
    };
    let ready = move || match (ticket.with(Option::is_some), store.gate.get().as_str()) {
        (true, _) => !code.get().trim().is_empty(),
        (false, "setup") => !password.get().is_empty() && !repeat.get().is_empty(),
        _ => !password.get().is_empty(),
    };
    let blocked = move || !ready() || busy.get() || waiting() > 0.0;
    let submit_button = move |label: &'static str| {
        view! {
            <button class="btn primary" type="submit" aria-busy=move || busy.get().to_string() aria-label=label disabled=blocked>
                {move || if busy.get() { view! { <span class="spinner" aria-hidden="true"></span> }.into_any() } else { label.into_any() }}
            </button>
        }
    };
    let form = move || match (ticket.with(Option::is_some), store.gate.get().as_str()) {
        (true, _) => view! {
            <h1>"Two-factor authentication"</h1>
            <p>"Enter the code from your authenticator app."</p>
            <div class=fields>{field(code, "6-digit code", "one-time-code", false)}</div>
            {submit_button("Verify")}
            <button class="btn ghost" type="button" on:click=move |_| back()>"Back"</button>
            {note()}
        }
        .into_any(),
        (false, "pending") => view! {
            <h1>"Waiting for the internal node"</h1>
            <p>"This node takes its password from the internal node with the next sync."</p>
            <button class="btn primary" type="button" on:click=move |_| store.check_session()>"Check again"</button>
        }
        .into_any(),
        (false, "expired") => view! {
            <h1>"Setup time is over"</h1>
            <p>"Restart the node or run argus-node reset-password on the server, then check again."</p>
            <button class="btn primary" type="button" on:click=move |_| store.check_session()>"Check again"</button>
        }
        .into_any(),
        (false, "setup") => view! {
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
            {move || submit_button(if store.two_factor.get() { "Continue" } else { "Sign in" })}
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

fn focus_field() {
    if let Some(input) = document().query_selector(".login-field input").ok().flatten().and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()) {
        let _ = input.focus();
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
                {nav_item("hosts", "Hosts", "#/hosts", icons::HOSTS, || None)}
                {nav_item("checks", "Services", "#/services", icons::CHECKS, || None)}
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
