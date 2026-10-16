use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::api;
use crate::components::segmented;
use crate::format;
use crate::icons;
use crate::route::Route;
use crate::store::{Store, use_store};
use crate::theme::Theme;

const HIDDEN: &str = "********";

const PAGES: [(&str, &str); 12] = [
    ("general", "General"),
    ("agents", "Hosts and agents"),
    ("checks", "Services"),
    ("rules", "Alert rules"),
    ("notifications", "Notifications"),
    ("peer", "External node"),
    ("backups", "ZeroByte integration"),
    ("retention", "Data retention"),
    ("dashboard", "Password"),
    ("appearance", "Appearance"),
    ("json", "JSON"),
    ("about", "About"),
];

const METRICS: [&str; 12] = [
    "cpu||total",
    "cpu||iowait",
    "memory||used",
    "fs|*|used",
    "load||1",
    "load||5",
    "load||15",
    "temp|*|value",
    "net|*|rx",
    "net|*|tx",
    "smart|*|used",
    "check|*|latency",
];

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn set(root: &mut Value, path: &str, value: Option<Value>) {
    let parts: Vec<String> = path.split('/').skip(1).map(|p| p.replace("~1", "/").replace("~0", "~")).collect();
    let Some((last, parents)) = parts.split_last() else {
        return;
    };
    let mut node = root;
    for (index, part) in parents.iter().enumerate() {
        let next = parents.get(index + 1).unwrap_or(last);
        let fresh = || if next.parse::<usize>().is_ok() { json!([]) } else { json!({}) };
        node = match node {
            Value::Object(map) => map.entry(part.clone()).or_insert_with(fresh),
            Value::Array(list) => match part.parse::<usize>() {
                Ok(at) if at < list.len() => &mut list[at],
                Ok(at) if at == list.len() => {
                    list.push(fresh());
                    &mut list[at]
                }
                _ => return,
            },
            _ => return,
        };
    }
    match (node, value) {
        (Value::Object(map), Some(value)) => {
            map.insert(last.clone(), value);
        }
        (Value::Object(map), None) => {
            map.remove(last);
        }
        (Value::Array(list), Some(value)) => match last.parse::<usize>() {
            Ok(index) if index < list.len() => list[index] = value,
            _ => list.push(value),
        },
        (Value::Array(list), None) => {
            if let Ok(index) = last.parse::<usize>()
                && index < list.len()
            {
                list.remove(index);
            }
        }
        _ => {}
    }
}

fn read(store: Store, path: &str) -> Option<Value> {
    store.config.with(|c| c.as_ref().and_then(|c| c.pointer(path)).filter(|v| !v.is_null()).cloned())
}

fn saved(store: Store, path: &str) -> Option<Value> {
    store.saved.with(|s| s.as_ref().and_then(|s| s.config.pointer(path)).filter(|v| !v.is_null()).cloned())
}

fn text(store: Store, path: &str) -> String {
    match read(store, path) {
        Some(Value::String(text)) => text,
        Some(Value::Number(number)) => match number.as_f64() {
            Some(value) if value.fract() == 0.0 && value.abs() < 1e15 => {
                format!("{}", value as i64)
            }
            _ => number.to_string(),
        },
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        Some(Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

fn write(store: Store, path: &str, value: Option<Value>) {
    store.config.update(|config| {
        if let Some(config) = config {
            set(config, path, value);
        }
    });
}

fn len(store: Store, path: &str) -> usize {
    read(store, path).map_or(0, |v| v.as_array().map_or(0, Vec::len))
}

fn keys(store: Store, path: &str) -> Vec<String> {
    read(store, path).and_then(|v| v.as_object().map(|m| m.keys().cloned().collect())).unwrap_or_default()
}

fn origin() -> String {
    window().location().origin().unwrap_or_default()
}

fn copy(store: Store, text: String) {
    let _ = window().navigator().clipboard().write_text(&text);
    store.notify("Copied");
}

fn fetch_token(then: impl FnOnce(String) + 'static) {
    spawn_local(async move {
        if let Ok(reply) = api::get::<Value>("/api/v1/token").await
            && let Some(token) = reply["token"].as_str()
        {
            then(token.to_string());
        }
    });
}

fn row(title: &'static str, sub: impl IntoView + 'static, control: impl IntoView + 'static) -> impl IntoView {
    view! {
        <div class="row">
            <div class="row-head">
                <div class="row-title">{title}</div>
                <p class="row-sub">{sub}</p>
            </div>
            <div class="ctl">{control}</div>
        </div>
    }
}

fn field(label: &'static str, control: impl IntoView + 'static) -> impl IntoView {
    view! {
        <label class="field">
            <span>{label}</span>
            {control}
        </label>
    }
}

fn text_input(store: Store, path: String, placeholder: &'static str, optional: bool) -> impl IntoView {
    let shown = path.clone();
    view! {
        <input
            class="input"
            type="text"
            spellcheck="false"
            placeholder=placeholder
            prop:value=move || text(store, &shown)
            on:input=move |ev| {
                let value = event_target_value(&ev);
                write(store, &path, if optional && value.trim().is_empty() { None } else { Some(Value::String(value)) });
            }
        />
    }
}

fn value_input(store: Store, path: String, placeholder: &'static str) -> impl IntoView {
    let shown = path.clone();
    view! {
        <input
            class="input"
            type="text"
            spellcheck="false"
            placeholder=placeholder
            prop:value=move || text(store, &shown)
            on:input=move |ev| {
                let raw = event_target_value(&ev);
                let value = match serde_json::from_str::<Value>(raw.trim()) {
                    Ok(parsed @ (Value::Bool(_) | Value::Number(_))) => Some(parsed),
                    _ if raw.trim().is_empty() => None,
                    _ => Some(Value::String(raw)),
                };
                write(store, &path, value);
            }
        />
    }
}

const UNITS: [(&str, &str); 4] = [("s", "seconds"), ("m", "minutes"), ("h", "hours"), ("d", "days")];

fn split_span(text: &str) -> (String, &'static str) {
    let text = text.trim();
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let unit = UNITS.iter().map(|(unit, _)| *unit).find(|unit| text[digits..].trim() == *unit).unwrap_or("s");
    (text[..digits].to_string(), unit)
}

fn span_input(store: Store, path: String, optional: bool) -> impl IntoView {
    let (number_path, unit_path, shown_number, shown_unit) = (path.clone(), path.clone(), path.clone(), path);
    let store_span = move |path: &str, number: String, unit: &str| {
        let value = if number.trim().is_empty() && optional { None } else { Some(json!(format!("{}{unit}", number.trim()))) };
        write(store, path, value);
    };
    view! {
        <div class="span-input">
            <input
                class="input"
                type="number"
                min="0"
                inputmode="numeric"
                aria-label="Amount"
                prop:value=move || split_span(&text(store, &shown_number)).0
                on:input=move |ev| {
                    let unit = split_span(&text(store, &number_path)).1;
                    store_span(&number_path, event_target_value(&ev), unit);
                }
            />
            <select
                class="input"
                aria-label="Unit"
                prop:value=move || split_span(&text(store, &shown_unit)).1
                on:change=move |ev| {
                    let number = split_span(&text(store, &unit_path)).0;
                    let unit = event_target_value(&ev);
                    store_span(&unit_path, if number.is_empty() { "1".to_string() } else { number }, &unit);
                }
            >
                {UNITS.iter().map(|(unit, label)| view! { <option value=*unit>{*label}</option> }).collect_view()}
            </select>
        </div>
    }
}

fn number_input(store: Store, path: String, placeholder: &'static str, optional: bool) -> impl IntoView {
    let shown = path.clone();
    view! {
        <input
            class="input"
            type="text"
            inputmode="decimal"
            placeholder=placeholder
            prop:value=move || text(store, &shown)
            on:change=move |ev| {
                let value = event_target_value(&ev);
                let number = value.trim().replace(',', ".").parse::<f64>().ok().and_then(|n| {
                    if n.fract() == 0.0 && n.abs() < 1e15 { Some(json!(n as i64)) } else { serde_json::Number::from_f64(n).map(Value::Number) }
                });
                match number {
                    Some(number) => write(store, &path, Some(number)),
                    None if optional && value.trim().is_empty() => write(store, &path, None),
                    None => store.notify("Enter a number"),
                }
            }
        />
    }
}

fn list_input(store: Store, path: String, placeholder: &'static str, fallback: Option<&'static str>) -> impl IntoView {
    let shown = path.clone();
    view! {
        <input
            class="input"
            type="text"
            spellcheck="false"
            placeholder=placeholder
            prop:value=move || text(store, &shown)
            on:change=move |ev| {
                let items: Vec<Value> = event_target_value(&ev).split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| json!(s)).collect();
                let items = if items.is_empty() { fallback.map(|f| vec![json!(f)]).unwrap_or_default() } else { items };
                write(store, &path, Some(Value::Array(items)));
            }
        />
    }
}

fn select_input(store: Store, path: String, options: &'static [(&'static str, &'static str)]) -> impl IntoView {
    let shown = path.clone();
    view! {
        <select class="input" prop:value=move || text(store, &shown) on:change=move |ev| write(store, &path, Some(json!(event_target_value(&ev))))>
            {options.iter().map(|(value, label)| view! { <option value=*value>{*label}</option> }).collect_view()}
        </select>
    }
}

fn password_input(store: Store, path: String) -> impl IntoView {
    let (shown, hint) = (path.clone(), path.clone());
    let placeholder = move || {
        if saved(store, &hint).is_some_and(|v| v == HIDDEN) { "Stored, type to replace" } else { "Not set" }
    };
    view! {
        <input
            class="input"
            type="password"
            autocomplete="new-password"
            placeholder=placeholder
            prop:value=move || { let value = text(store, &shown); if value == HIDDEN { String::new() } else { value } }
            on:input=move |ev| {
                let value = event_target_value(&ev);
                let stored = saved(store, &path).is_some_and(|v| v == HIDDEN);
                write(store, &path, Some(json!(if value.is_empty() && stored { HIDDEN.to_string() } else { value })));
            }
        />
    }
}

fn token_input(store: Store, path: String, optional: bool, fresh: RwSignal<Option<String>>) -> impl IntoView {
    let (shown, hint, generate) = (path.clone(), path.clone(), path.clone());
    let value = Memo::new(move |_| match text(store, &shown) {
        value if value == HIDDEN => fresh.get().unwrap_or_default(),
        value => value,
    });
    let current = move || Some(value.get()).filter(|v| !v.is_empty());
    view! {
        <div class="ctl-stack">
            <input
                class="input mono"
                type="text"
                spellcheck="false"
                placeholder=move || if saved(store, &hint).is_some_and(|v| v == HIDDEN) { "Stored, generate or type to replace" } else { "Not set" }
                prop:value=value
                on:input=move |ev| {
                    let value = event_target_value(&ev);
                    fresh.set(Some(value.clone()).filter(|v| !v.is_empty()));
                    let stored = saved(store, &path).is_some_and(|v| v == HIDDEN);
                    let next = match (value.is_empty(), stored, optional) {
                        (true, true, _) => Some(json!(HIDDEN)),
                        (true, false, true) => None,
                        _ => Some(json!(value)),
                    };
                    write(store, &path, next);
                }
            />
            <div class="ctl-row">
                <button class="btn small" type="button" on:click=move |_| {
                    let path = generate.clone();
                    fetch_token(move |token| {
                        write(store, &path, Some(json!(token.clone())));
                        fresh.set(Some(token));
                    });
                }>
                    <span class="ico" inner_html=icons::REFRESH></span>"Generate"
                </button>
                {move || current().map(|token| view! {
                    <button class="btn small" type="button" on:click=move |_| copy(store, token.clone())>
                        <span class="ico" inner_html=icons::COPY></span>"Copy"
                    </button>
                })}
            </div>
        </div>
    }
}

fn switch(store: Store, path: &'static str, template: fn() -> Value) -> impl IntoView {
    let on = move || read(store, path).is_some();
    view! {
        <button
            class="switch"
            type="button"
            role="switch"
            aria-checked=move || on().to_string()
            on:click=move |_| {
                if on() && !window().confirm_with_message("Turn this off? Its stored keys and tokens are deleted.").unwrap_or(false) {
                    return;
                }
                let next = if on() { None } else { Some(saved(store, path).unwrap_or_else(template)) };
                write(store, path, next);
            }
        >
            <span></span>
        </button>
    }
}

fn group(title: &'static str) -> impl IntoView {
    view! { <h3 class="group-head">{title}</h3> }
}

fn general(store: Store) -> impl IntoView {
    let path = move || store.saved.with(|s| s.as_ref().and_then(|s| s.path.clone())).unwrap_or_else(|| "Not saved to a file".to_string());
    view! {
        <p class="lead">"Changes save on their own. The config file stays in sync, edits there apply within seconds."</p>
        {row("Node name", "Agents name it as their site. Each node needs its own name.", text_input(store, "/name".into(), "internal", false))}
        {row("Listen address", "Takes effect after a restart.", text_input(store, "/listen".into(), "0.0.0.0:8090", false))}
        {row("Database", "Takes effect after a restart.", text_input(store, "/database".into(), "argus.db", false))}
        {row(
            "Connections",
            "Persistent keeps a connection open from every agent to each node and between the nodes, so the nodes pass on every change at once. A lost connection is rebuilt on its own, meanwhile everything goes by request.",
            select_input(store, "/shared/connection".into(), &[("interval", "Interval"), ("persistent", "Persistent")]),
        )}
        {row("Config file", "", view! { <span class="mono value">{path}</span> })}
    }
}

fn agents(store: Store) -> impl IntoView {
    let fresh = RwSignal::new(None::<(String, String)>);
    let new_id = RwSignal::new(String::new());
    let ids = Memo::new(move |_| {
        let order: Vec<String> = read(store, "/shared/order").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        let mut ids = keys(store, "/shared/agents");
        ids.sort_by_key(|id| order.iter().position(|o| o == id).unwrap_or(usize::MAX));
        ids
    });
    let add = move |_| {
        let id = new_id.get_untracked().trim().to_string();
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
            store.notify("Use letters, digits, dash, dot or underscore for the host id");
            return;
        }
        if ids.with_untracked(|ids| ids.contains(&id)) {
            store.notify("This host already exists");
            return;
        }
        fetch_token(move |token| {
            write(store, &format!("/shared/agents/{}", escape(&id)), Some(json!(token.clone())));
            fresh.set(Some((id, token)));
            new_id.set(String::new());
        });
    };
    let item = move |id: String| {
        let (renew, remove, status) = (id.clone(), id.clone(), id.clone());
        view! {
            <li class="entry">
                <span class="tile"><span class="ico" inner_html=icons::HOSTS></span></span>
                <span class="li-text">
                    <span class="li-title mono">{id.clone()}</span>
                    <span class="li-sub">{move || if text(store, &format!("/shared/agents/{}", escape(&status))) == HIDDEN { "Token stored" } else { "New token" }}</span>
                </span>
                <button class="btn small" type="button" on:click=move |_| {
                    let id = renew.clone();
                    fetch_token(move |token| {
                        write(store, &format!("/shared/agents/{}", escape(&id)), Some(json!(token.clone())));
                        fresh.set(Some((id, token)));
                    });
                }>"New token"</button>
                <button class="btn small icon danger" type="button" aria-label=format!("Remove {id}") on:click=move |_| {
                    write(store, &format!("/shared/agents/{}", escape(&remove)), None);
                    write(store, &format!("/shared/checks/{}", escape(&remove)), None);
                    if fresh.with_untracked(|f| f.as_ref().is_some_and(|(id, _)| *id == remove)) {
                        fresh.set(None);
                    }
                }>
                    <span class="ico" inner_html=icons::TRASH></span>
                </button>
            </li>
        }
    };
    let snippet = move || {
        fresh.get().map(|(id, token)| {
            let config = format!("id = \"{id}\"\nsite = \"{}\"\n\n[[nodes]]\nurl = \"{}\"\ntoken = \"{token}\"", text(store, "/name"), origin());
            let copied = config.clone();
            view! {
                <div class="callout">
                    <div class="callout-head">
                        <b>{format!("Token for {id}")}</b>
                        <button class="btn small" type="button" on:click=move |_| copy(store, copied.clone())>
                            <span class="ico" inner_html=icons::COPY></span>"Copy"
                        </button>
                    </div>
                    <p>"Shown once. Put it into the agent config, and add the second node under [[nodes]] too if you run one."</p>
                    <pre class="code">{config}</pre>
                </div>
            }
        })
    };
    view! {
        <p class="lead">"Every agent sends its reports with its own token. The node only keeps the sha256 of each token."</p>
        <div class="add-row">
            <input class="input" type="text" spellcheck="false" placeholder="Host id, for example nas" prop:value=move || new_id.get() on:input=move |ev| new_id.set(event_target_value(&ev)) on:keydown=move |ev| if ev.key() == "Enter" { add(()) }/>
            <button class="btn" type="button" on:click=move |_| add(())><span class="ico" inner_html=icons::PLUS></span>"Add host"</button>
        </div>
        {snippet}
        <ul class="entries">
            <For each=move || ids.get() key=|id| id.clone() children=item/>
        </ul>
        <Show when=move || ids.with(Vec::is_empty)>
            <p class="empty-note">"No agents yet. Reports from unknown tokens are rejected."</p>
        </Show>
    }
}

fn check_template(kind: &str, name: String) -> Value {
    match kind {
        "tcp" => json!({ "name": name, "type": "tcp", "host": "", "port": 443 }),
        "ping" => json!({ "name": name, "type": "ping", "host": "" }),
        _ => json!({ "name": name, "type": "http", "url": "https://", "status": 200 }),
    }
}

fn check_card(store: Store, base: String, runner: Option<Memo<Vec<String>>>) -> impl IntoView {
    let kind_path = format!("{base}/type");
    let kind = Memo::new(move |_| text(store, &kind_path));
    let (retype, remove, name) = (base.clone(), base.clone(), base.clone());
    let fields = move || {
        let p = |field: &str| format!("{base}/{field}");
        match kind.get().as_str() {
            "tcp" => view! {
                {field("Host", text_input(store, p("host"), "db.example.com", false))}
                {field("Port", number_input(store, p("port"), "5432", false))}
                {field("Timeout", span_input(store, p("timeout"), true))}
            }
            .into_any(),
            "ping" => view! { {field("Host", text_input(store, p("host"), "192.0.2.1", false))} }.into_any(),
            _ => view! {
                {field("URL", text_input(store, p("url"), "https://service.example.com/health", false))}
                {field("Expected status", number_input(store, p("status"), "any 2xx", true))}
                {field("Body contains", text_input(store, p("contains"), "(optional)", true))}
                {field("Timeout", span_input(store, p("timeout"), true))}
                {field("JSON field", json_path_input(store, p("json")))}
                {field("Equals", value_input(store, p("json/0/equals"), "ok, true or 200"))}
            }
            .into_any(),
        }
    };
    view! {
        <div class="card">
            <div class="card-grid">
                {field("Name", text_input(store, format!("{name}/name"), "Website", false))}
                {field("Type", view! {
                    <select class="input" prop:value=move || kind.get() on:change=move |ev| {
                        let name = text(store, &format!("{retype}/name"));
                        write(store, &retype, Some(check_template(&event_target_value(&ev), name)));
                    }>
                        <option value="http">"HTTP"</option>
                        <option value="tcp">"TCP port"</option>
                        <option value="ping">"Ping"</option>
                    </select>
                })}
                {fields}
                {runner.map(|agents| {
                    let (shown, path) = (format!("{remove}/runner"), format!("{remove}/runner"));
                    field("Run by", view! {
                        <select class="input" prop:value=move || text(store, &shown) on:change=move |ev| {
                            let value = event_target_value(&ev);
                            write(store, &path, (!value.is_empty()).then(|| json!(value)));
                        }>
                            <option value="">"Any home agent"</option>
                            {move || agents.get().into_iter().map(|id| view! { <option value=id.clone()>{id.clone()}</option> }).collect_view()}
                        </select>
                    })
                })}
            </div>
            <button class="btn small danger card-remove" type="button" aria-label="Remove service" on:click=move |_| write(store, &remove, None)>
                <span class="ico" inner_html=icons::TRASH></span>
            </button>
        </div>
    }
}

fn checks(store: Store) -> impl IntoView {
    let hosts = Memo::new(move |_| keys(store, "/shared/agents"));
    let used = Memo::new(move |_| hosts.get().into_iter().filter(|host| len(store, &format!("/shared/checks/{}", escape(host))) > 0).collect::<Vec<_>>());
    let standalone = Memo::new(move |_| len(store, "/shared/services"));
    let has_standalone = move || standalone.get() != 0;
    let chosen = RwSignal::new(String::new());
    let add = move |_| {
        let host = chosen.get_untracked();
        let path = if host.is_empty() { "/shared/services".to_string() } else { format!("/shared/checks/{}", escape(&host)) };
        let mut list = read(store, &path).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        list.push(check_template("http", format!("Service {}", list.len() + 1)));
        write(store, &path, Some(Value::Array(list)));
    };
    let host_block = move |host: String| {
        let path = format!("/shared/checks/{}", escape(&host));
        let count = Memo::new(move |_| len(store, &path));
        let card_host = host.clone();
        view! {
            <section class="block">
                <h3 class="group-head">{format!("Run by {host}")}</h3>
                <For each=move || 0..count.get() key=|i| *i children=move |i| check_card(store, format!("/shared/checks/{}/{i}", escape(&card_host)), None)/>
            </section>
        }
    };
    view! {
        <p class="lead">"HTTP, TCP and ping checks are sent to the agent that runs them with its next report. A standalone service belongs to no host, like a camera without an agent: any home agent runs it unless you pick one, and it shows and alerts on its own. Commands and file checks stay in the agent's own config, the node can never start a program on a host."</p>
        <Show when=move || hosts.with(|h| !h.is_empty()) fallback=|| view! { <p class="empty-note">"Add a host under Hosts and agents first."</p> }>
            <div class="add-row">
                <select class="input" aria-label="Where the service belongs" prop:value=move || chosen.get() on:change=move |ev| chosen.set(event_target_value(&ev))>
                    <option value="">"Standalone service"</option>
                    {move || hosts.get().into_iter().map(|host| view! { <option value=host.clone()>{format!("Part of {host}")}</option> }).collect_view()}
                </select>
                <button class="btn" type="button" on:click=add><span class="ico" inner_html=icons::PLUS></span>"Add service"</button>
            </div>
        </Show>
        <Show when=has_standalone>
            <section class="block">
                <h3 class="group-head">"Standalone"</h3>
                <For each=move || 0..standalone.get() key=|i| *i children=move |i| check_card(store, format!("/shared/services/{i}"), Some(hosts))/>
            </section>
        </Show>
        <For each=move || used.get() key=|h| h.clone() children=host_block/>
        <Show when=move || hosts.with(|h| !h.is_empty()) && used.with(Vec::is_empty) && standalone.get() == 0>
            <p class="empty-note">"No central services yet."</p>
        </Show>
    }
}

fn json_path_input(store: Store, path: String) -> impl IntoView {
    let shown = format!("{path}/0/path");
    view! {
        <input
            class="input"
            type="text"
            spellcheck="false"
            placeholder="status (optional)"
            prop:value=move || text(store, &shown)
            on:input=move |ev| {
                let value = event_target_value(&ev);
                if value.trim().is_empty() {
                    write(store, &path, None);
                } else {
                    write(store, &format!("{path}/0/path"), Some(Value::String(value)));
                }
            }
        />
    }
}

fn rule_card(store: Store, index: usize) -> impl IntoView {
    let base = format!("/shared/rules/{index}");
    let (above, below, remove) = (format!("{base}/above"), format!("{base}/below"), base.clone());
    let direction = {
        let below = below.clone();
        Memo::new(move |_| if read(store, &below).is_some() { "below" } else { "above" })
    };
    let limit_path = Memo::new(move |_| format!("{base}/{}", direction.get()));
    let p = move |field: &str| format!("/shared/rules/{index}/{field}");
    view! {
        <div class="card">
            <div class="card-grid">
                {field("Name", text_input(store, p("name"), "CPU", false))}
                {field("Metric", view! {
                    <input class="input mono" type="text" list="metrics" spellcheck="false" placeholder="cpu||total"
                        prop:value=move || text(store, &p("metric"))
                        on:input=move |ev| write(store, &p("metric"), Some(json!(event_target_value(&ev))))/>
                })}
                {field("Hosts", list_input(store, p("hosts"), "*, !nas", Some("*")))}
                {field("Condition", view! {
                    <div class="ctl-row">
                        <select class="input narrow" prop:value=move || direction.get() on:change=move |ev| {
                            let value = read(store, &format!("/shared/rules/{index}/{}", direction.get_untracked())).unwrap_or(json!(85));
                            write(store, &above, None);
                            write(store, &below, None);
                            write(store, &format!("/shared/rules/{index}/{}", event_target_value(&ev)), Some(value));
                        }>
                            <option value="above">"Above"</option>
                            <option value="below">"Below"</option>
                        </select>
                        {move || number_input(store, limit_path.get(), "85", false)}
                    </div>
                })}
                {field("For at least", span_input(store, p("for"), true))}
                {field("Severity", select_input(store, p("severity"), &[("warning", "Warning"), ("critical", "Critical")]))}
            </div>
            <button class="btn small danger card-remove" type="button" aria-label="Remove rule" on:click=move |_| write(store, &remove, None)>
                <span class="ico" inner_html=icons::TRASH></span>
            </button>
        </div>
    }
}

fn rules(store: Store) -> impl IntoView {
    let count = Memo::new(move |_| len(store, "/shared/rules"));
    let add = move |_| {
        let mut list = read(store, "/shared/rules").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        list.push(json!({ "name": "New rule", "metric": "cpu||total", "hosts": ["*"], "above": 90, "for": "5m", "severity": "warning" }));
        write(store, "/shared/rules", Some(Value::Array(list)));
    };
    view! {
        <p class="lead">"Metrics are group|item|field, * matches anything. Hosts take patterns and !exclusions. Unreachable hosts, failing services, stopped containers, RAID, SMART, systemd and reboots always alert."</p>
        <datalist id="metrics">{METRICS.iter().map(|m| view! { <option value=*m></option> }).collect_view()}</datalist>
        <div class="add-row">
            <button class="btn" type="button" on:click=add><span class="ico" inner_html=icons::PLUS></span>"Add rule"</button>
        </div>
        <For each=move || 0..count.get() key=|i| *i children=move |i| rule_card(store, i)/>
    }
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = argusPush, js_name = state)]
    fn push_state() -> String;
    #[wasm_bindgen(catch, js_namespace = argusPush, js_name = subscribe)]
    async fn push_subscribe(key: &str) -> Result<JsValue, JsValue>;
}

fn push_template() -> Value {
    let origin = origin();
    let subject = if origin.starts_with("https://") { origin.clone() } else { String::new() };
    json!({ "subject": subject, "receipt_urls": [origin], "devices": [] })
}

fn this_device(store: Store) -> impl IntoView {
    let name = RwSignal::new("iPhone".to_string());
    let busy = RwSignal::new(false);
    let enable = move |_| {
        busy.set(true);
        let device = name.get_untracked();
        spawn_local(async move {
            let key = if store.settled().await { saved(store, "/shared/push/public_key").and_then(|v| v.as_str().map(str::to_string)) } else { None };
            let Some(key) = key.filter(|k| !k.is_empty()) else {
                store.notify("Not enabled, the settings could not be saved");
                busy.set(false);
                return;
            };
            let result = match push_subscribe(&key).await {
                Ok(value) => {
                    let mut body: Value = value.as_string().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
                    body["name"] = json!(device);
                    api::send("/api/v1/push/devices", &body).await.map_err(|failure| failure.text)
                }
                Err(error) => {
                    Err(js_sys::Reflect::get(&error, &"message".into()).ok().and_then(|m| m.as_string()).unwrap_or_else(|| "unknown error".to_string()))
                }
            };
            match result {
                Ok(()) => {
                    store.notify("This device gets notifications now");
                    store.load_config();
                }
                Err(error) => store.notify(format!("Not enabled, {error}")),
            }
            busy.set(false);
        });
    };
    match push_state().as_str() {
        "ready" => view! {
            <div class="ctl-row">
                <input class="input" type="text" aria-label="Device name" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
                <button class="btn primary" type="button" disabled=move || busy.get() on:click=enable>"Enable"</button>
            </div>
        }
        .into_any(),
        "install" => {
            view! { <span class="value">"Open this page in Safari, tap Share, Add to Home Screen, then open Argus from the home screen"</span> }.into_any()
        }
        "denied" => view! { <span class="value">"Notifications for Argus are blocked in the system settings"</span> }.into_any(),
        "insecure" => view! { <span class="value">"Needs the dashboard over HTTPS"</span> }.into_any(),
        _ => view! { <span class="value">"This browser cannot receive notifications"</span> }.into_any(),
    }
}

fn notifications(store: Store) -> impl IntoView {
    let enabled = move || read(store, "/shared/push").is_some();
    let devices = Memo::new(move |_| len(store, "/shared/push/devices"));
    let device = move |index: usize| {
        let base = format!("/shared/push/devices/{index}");
        let (title, added, remove, named) = (format!("{base}/name"), format!("{base}/added"), base.clone(), format!("{base}/name"));
        let test = move |_| {
            let name = text(store, &named);
            spawn_local(async move {
                if !store.settled().await {
                    store.notify("Not sent, the settings could not be saved");
                    return;
                }
                match api::send("/api/v1/notify-test", &json!({ "device": name })).await {
                    Ok(()) => store.notify(format!("Test notification sent to {name}")),
                    Err(failure) => store.notify(format!("Test failed, {failure}")),
                }
            });
        };
        view! {
            <li class="entry">
                <span class="tile"><span class="ico" inner_html=icons::ALERTS></span></span>
                <span class="li-text">
                    <span class="li-title">{move || text(store, &title)}</span>
                    <span class="li-sub">{move || {
                        let at = read(store, &added).and_then(|v| v.as_f64()).unwrap_or_default();
                        if at > 0.0 { format!("Added {}", format::ago(at, store.clock.get())) } else { "Added".to_string() }
                    }}</span>
                </span>
                <button class="btn small" type="button" on:click=test>"Test"</button>
                <button class="btn small icon danger" type="button" aria-label="Remove device" on:click=move |_| write(store, &remove, None)>
                    <span class="ico" inner_html=icons::TRASH></span>
                </button>
            </li>
        }
    };
    view! {
        <p class="lead">"Argus encrypts every notification and hands it straight to the push service of your phone, Apple on an iPhone. It only sees encrypted data, the phone decrypts, shows the text and confirms receipt."</p>
        {row("Phone notifications", "Shared by both nodes, either one can notify.", switch(store, "/shared/push", push_template))}
        <Show when=enabled>
            {row("Contact", "Apple asks every sender for a mailto: or https:// address.", text_input(store, "/shared/push/subject".into(), "mailto:you@example.com", false))}
            {row("Confirmation addresses", "Where the phone confirms receipt, this dashboard and the node that is reachable from outside.", list_input(store, "/shared/push/receipt_urls".into(), "https://argus.internal.example.com", None))}
            {row("Status message", "A short daily all clear proves the path works. Empty turns it off.", span_input(store, "/shared/push/status_every".into(), true))}
            {row("This device", "Run once per phone, from the installed Argus app.", this_device(store))}
            {group("Devices")}
            <ul class="entries">
                <For each=move || 0..devices.get() key=|i| *i children=device/>
            </ul>
            <Show when=move || devices.get() == 0>
                <p class="empty-note">"No device yet."</p>
            </Show>
        </Show>
    }
}

fn peer_label(store: Store) -> &'static str {
    match (read(store, "/peer").is_some(), read(store, "/peer/url").is_some()) {
        (true, false) => "Internal node",
        _ => "External node",
    }
}

fn peer(store: Store) -> impl IntoView {
    let enabled = move || read(store, "/peer").is_some();
    let internal = move || read(store, "/peer/url").is_some();
    let role = move |ev| {
        let to_internal = event_target_value(&ev) == "internal";
        let address = saved(store, "/peer/url").filter(|_| to_internal).unwrap_or_else(|| json!(""));
        write(store, "/peer/url", to_internal.then_some(address));
        if matches!(text(store, "/peer/name").as_str(), "" | "internal" | "external") {
            write(store, "/peer/name", Some(json!(if to_internal { "external" } else { "internal" })));
        }
    };
    view! {
        <p class="lead">"Two nodes watch each other. The internal node starts every sync, so nothing ever connects into your network. After 60 s without sync each node takes over the hosts it still hears from."</p>
        {row("Two nodes", "Turn off to run a single node.", switch(store, "/peer", || json!({ "name": "external", "url": "", "token": "" })))}
        <Show when=enabled>
            {row("This node", "The internal node runs at home, the external one is reachable from the internet.", view! {
                <select class="input" prop:value=move || if internal() { "internal" } else { "external" } on:change=role>
                    <option value="internal">"Internal"</option>
                    <option value="external">"External"</option>
                </select>
            })}
            {row("Other node's name", "Its node name under General.", text_input(store, "/peer/name".into(), "external", false))}
            <Show when=internal>
                {row("Address", "Where this node reaches the external node.", text_input(store, "/peer/url".into(), "https://argus.external.example.com", false))}
            </Show>
            {row("Sync token", "The same secret on both nodes, at least 16 characters.", token_input(store, "/peer/token".into(), false, RwSignal::new(None)))}
        </Show>
    }
}

fn backups(store: Store) -> impl IntoView {
    let enabled = move || read(store, "/zerobyte").is_some();
    let fresh = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        if enabled() && read(store, "/zerobyte/webhook_token").is_none() {
            fetch_token(move |token| {
                write(store, "/zerobyte/webhook_token", Some(json!(token.clone())));
                fresh.set(Some(token));
            });
        }
    });
    let hook = move || match (text(store, "/zerobyte/webhook_token"), fresh.get()) {
        (token, _) if token.is_empty() => "Generate a token to get the webhook address for ZeroByte.".to_string(),
        (token, Some(fresh)) if token == HIDDEN => format!("{}/api/v1/hooks/zerobyte/{fresh}", origin()),
        (token, None) if token == HIDDEN => format!("{}/api/v1/hooks/zerobyte/<stored token>", origin()),
        (token, _) => format!("{}/api/v1/hooks/zerobyte/{token}", origin()),
    };
    view! {
        <p class="lead">"Argus reads every backup schedule and its mirrors from the ZeroByte API and receives every backup run from a ZeroByte webhook within seconds. It alerts when a backup or mirror fails or no success arrived in time. Schedules deleted in ZeroByte disappear here on their own. Argus only reads and receives, it never starts or changes a backup."</p>
        {row("ZeroByte integration", "Show backups and alert on problems.", switch(store, "/zerobyte", || json!({ "interval": "5m" })))}
        <Show when=enabled>
            {group("API")}
            {row("API address", "Where this node reaches ZeroByte.", text_input(store, "/zerobyte/url".into(), "http://zerobyte.example.com:4096", false))}
            {row("API key", "ZeroByte keys can also delete backups, keep this node private.", password_input(store, "/zerobyte/api_key".into()))}
            {row("Ask every", "", span_input(store, "/zerobyte/interval".into(), false))}
            {group("Webhook")}
            {row("Webhook token", view! { <span class="mono">{hook}</span> }, token_input(store, "/zerobyte/webhook_token".into(), false, fresh))}
        </Show>
        {row("Alert after", "Alert when a backup or mirror fails or no success arrived for this long.", span_input(store, "/shared/backup_max_age".into(), false))}
    }
}

fn keep(store: Store, label: &'static str, sub: &'static str, level: &'static str) -> impl IntoView {
    let path = format!("/shared/retention/{level}");
    let shown = path.clone();
    let days = move || argus_model::parse_duration(&text(store, &shown)).map(|d| (d.as_secs() / 86_400).to_string()).unwrap_or_default();
    view! {
        <label class="field">
            <span>{label}</span>
            <input
                class="input"
                type="number"
                min="1"
                inputmode="numeric"
                prop:value=days
                on:input=move |ev| {
                    if let Ok(days) = event_target_value(&ev).trim().parse::<u32>() {
                        write(store, &path, Some(json!(format!("{days}d"))));
                    }
                }
            />
            <small class="field-sub">{sub}</small>
        </label>
    }
}

fn retention(store: Store) -> impl IntoView {
    view! {
        <p class="lead">"Older data is averaged into coarser points, so long time ranges stay small and fast. Each level keeps its points for the given number of days."</p>
        <div class="card plain">
            <div class="card-grid">
                {keep(store, "Keep every report", "Days with full detail.", "raw")}
                {keep(store, "Keep 5 minute averages", "Days of 5 minute points.", "medium")}
                {keep(store, "Keep hourly averages", "Days of hourly points.", "long")}
                {keep(store, "Keep daily averages", "Days of daily points.", "daily")}
            </div>
        </div>
    }
}

fn dashboard(store: Store) -> impl IntoView {
    let current = RwSignal::new(String::new());
    let next = RwSignal::new(String::new());
    let repeat = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let change = move |_| {
        if next.get_untracked() != repeat.get_untracked() {
            store.notify("The new passwords do not match");
            return;
        }
        busy.set(true);
        let body = json!({ "current": current.get_untracked(), "password": next.get_untracked() });
        spawn_local(async move {
            match api::send("/api/v1/password", &body).await {
                Ok(()) => {
                    store.notify("Password changed, other sessions were signed out");
                    for field in [current, next, repeat] {
                        field.set(String::new());
                    }
                }
                Err(failure) => store.notify(format!("Not changed, {failure}")),
            }
            busy.set(false);
        });
    };
    let input = move |value: RwSignal<String>, autocomplete: &'static str| {
        view! {
            <input class="input" type="password" autocomplete=autocomplete prop:value=move || value.get() on:input=move |ev| value.set(event_target_value(&ev))/>
        }
    };
    let follower = move || read(store, "/peer").is_some() && read(store, "/peer/url").is_none();
    view! {
        <p class="lead">"One password protects the dashboard on both nodes. Only its argon2 hash is stored, in the database and never in the config file. The internal node shares it with the external one. If it is lost, run argus-node reset-password on the internal node and set a new one within 15 minutes."</p>
        <Show when=move || !follower() fallback=|| view! { <p class="empty-note">"Change the password on the internal node, it reaches this node with the next sync."</p> }>
        {row("Current password", "", input(current, "current-password"))}
        {row("New password", "At least 10 characters.", input(next, "new-password"))}
        {row("Repeat new password", "", input(repeat, "new-password"))}
        {row("", "Changing the password signs out every session on both nodes.", view! {
            <button class="btn primary" type="button" disabled=move || busy.get() on:click=change>"Change password"</button>
        })}
        </Show>
    }
}

fn appearance(store: Store) -> impl IntoView {
    view! { {row("Theme", "Stored in this browser.", segmented(store.theme, Theme::OPTIONS, "Theme"))} }
}

fn json_editor(store: Store) -> impl IntoView {
    let pretty = move || store.config.with_untracked(|c| c.as_ref().and_then(|c| serde_json::to_string_pretty(c).ok())).unwrap_or_default();
    let draft = RwSignal::new(pretty());
    let error = RwSignal::new(String::new());
    let apply = move |_| match serde_json::from_str::<Value>(&draft.get_untracked()) {
        Ok(value) if value.is_object() => {
            store.config.set(Some(value));
            error.set(String::new());
            store.notify("Applied");
        }
        Ok(_) => error.set("The configuration must be a JSON object".to_string()),
        Err(problem) => error.set(problem.to_string()),
    };
    view! {
        <p class="lead">"The whole configuration as stored in the file. Secrets show as ******** and keep their stored value."</p>
        <textarea class="input code-edit" spellcheck="false" prop:value=move || draft.get() on:input=move |ev| draft.set(event_target_value(&ev))></textarea>
        <p class="form-error" role="alert">{move || error.get()}</p>
        <div class="add-row">
            <button class="btn" type="button" on:click=apply>"Apply"</button>
            <button class="btn" type="button" on:click=move |_| { draft.set(pretty()); error.set(String::new()); }>"Reset"</button>
        </div>
    }
}

fn about(store: Store) -> impl IntoView {
    let node = move || store.overview.with(|o| o.as_ref().map(|o| o.node.clone())).unwrap_or_default();
    view! {
        {row("Version", "", view! { <span class="value">{env!("CARGO_PKG_VERSION")}</span> })}
        {row("Node", "", view! { <span class="value">{node}</span> })}
        {row("License", "", view! { <span class="value">"PolyForm Noncommercial 1.0.0"</span> })}
        {row("Source", "", view! { <a class="value link" href="https://github.com/antndev/argus" target="_blank" rel="noreferrer">"github.com/antndev/argus"</a> })}
    }
}

#[component]
pub fn SettingsPage() -> impl IntoView {
    let store = use_store();
    if !store.dirty() {
        store.load_config();
    }
    let page = Memo::new(move |_| match store.route.get() {
        Route::Settings(page) if PAGES.iter().any(|(id, _)| *id == page) => page,
        _ => "general".to_string(),
    });
    let label = move |id: &str, fallback: &'static str| if id == "peer" { peer_label(store) } else { fallback };
    let title = move || PAGES.iter().find(|(id, _)| *id == page.get()).map_or("", |(id, fallback)| label(id, fallback));
    let content = move || {
        if store.config.with(Option::is_none) {
            return view! { <p class="empty-note">"Loading the settings"</p> }.into_any();
        }
        match page.get().as_str() {
            "agents" => agents(store).into_any(),
            "checks" => checks(store).into_any(),
            "rules" => rules(store).into_any(),
            "notifications" => notifications(store).into_any(),
            "peer" => peer(store).into_any(),
            "backups" => backups(store).into_any(),
            "retention" => retention(store).into_any(),
            "dashboard" => dashboard(store).into_any(),
            "appearance" => appearance(store).into_any(),
            "json" => json_editor(store).into_any(),
            "about" => about(store).into_any(),
            _ => general(store).into_any(),
        }
    };
    let loaded = Memo::new(move |_| store.config.with(Option::is_some));
    let chosen = move || store.route.with(|r| matches!(r, Route::Settings(page) if !page.is_empty()));
    view! {
        <div class="split" class:has-detail=chosen>
            <nav class="split-list narrow" aria-label="Settings sections">
                {PAGES
                    .iter()
                    .map(|(id, fallback)| {
                        let current = move || (page.get() == *id).then_some("page");
                        view! { <a class="cat" href=Route::Settings(id.to_string()).href() aria-current=current>{move || label(id, fallback)}</a> }
                    })
                    .collect_view()}
                <button class="cat sign-out" type="button" on:click=move |_| store.logout()>"Sign out"</button>
            </nav>
            <section class="split-detail" aria-label="Settings">
                <div class="settings">
                    <a class="back" href="#/settings"><span class="ico" inner_html=crate::icons::BACK></span>"Settings"</a>
                    <h2>{title}</h2>
                    {move || store.save_error.get().map(|error| view! { <p class="save-error" role="alert">{format!("Not saved, {error}")}</p> })}
                    {move || {
                        let _ = (loaded.get(), page.get());
                        untrack(content)
                    }}
                </div>
            </section>
        </div>
    }
}
