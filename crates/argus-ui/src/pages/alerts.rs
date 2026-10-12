use argus_model::{Event, Mute, MuteRequest, RuleInfo, Severity};
use leptos::ev::SubmitEvent;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{AlertList, Pill, sev_icon};
use crate::format;
use crate::store::use_store;

const DURATIONS: [(&str, &str); 5] = [("60", "1 hour"), ("240", "4 hours"), ("morning", "Until tomorrow 08:00"), ("1440", "24 hours"), ("10080", "7 days")];

fn tomorrow_morning() -> i64 {
    let date = js_sys::Date::new_0();
    date.set_date(date.get_date() + 1);
    date.set_hours(8);
    date.set_minutes(0);
    date.set_seconds(0);
    date.set_milliseconds(0);
    date.get_time() as i64
}

#[component]
pub fn AlertsPage() -> impl IntoView {
    let store = use_store();
    let target = RwSignal::new(String::new());
    let duration = RwSignal::new("240".to_string());
    let reason = RwSignal::new(String::new());
    let rules = RwSignal::new(Vec::<RuleInfo>::new());
    let duration_select = NodeRef::<html::Select>::new();

    store.refresh_events();
    spawn_local(async move {
        if let Ok(list) = api::get::<Vec<RuleInfo>>("/api/v1/rules").await {
            let _ = rules.try_set(list);
        }
    });
    let refresh = Memo::new(move |_| (store.clock.get() / 10_000.0).floor() as i64);
    Effect::new(move |previous: Option<()>| {
        refresh.track();
        if previous.is_some() {
            store.refresh_events();
        }
    });

    let targets = Memo::new(move |_| {
        let mut list: Vec<(String, String)> = store
            .alerts()
            .into_iter()
            .filter(|(_, mute)| mute.is_none())
            .map(|(alert, _)| (format!("a:{}", alert.key), format!("{}, {}", alert.host, alert.title)))
            .collect();
        list.extend(store.host_ids().into_iter().map(|id| (format!("h:{id}"), format!("All alerts on {id}"))));
        list
    });

    Effect::new(move |_| {
        let list = targets.get();
        if let Some(key) = store.mute_target.get() {
            target.set(format!("a:{key}"));
            store.mute_target.set(None);
            if let Some(select) = duration_select.get_untracked() {
                let _ = select.focus();
            }
            return;
        }
        let current = target.get_untracked();
        if !list.iter().any(|(value, _)| *value == current) {
            target.set(list.first().map(|(value, _)| value.clone()).unwrap_or_default());
        }
    });

    let submit = move |event: SubmitEvent| {
        event.prevent_default();
        let chosen = target.get_untracked();
        let (alert, host) = match (chosen.strip_prefix("a:"), chosen.strip_prefix("h:")) {
            (Some(key), _) => (Some(key.to_string()), None),
            (_, Some(id)) => (None, Some(id.to_string())),
            _ => return,
        };
        let length = duration.get_untracked();
        let (minutes, until) = match length.as_str() {
            "morning" => (None, Some(tomorrow_morning())),
            value => (value.parse().ok(), None),
        };
        store.mute(MuteRequest { alert, host, minutes, until, reason: reason.get_untracked().trim().to_string() });
        reason.set(String::new());
    };

    let target_options = move || {
        targets
            .get()
            .into_iter()
            .map(|(value, label)| {
                let selected = value.clone();
                view! { <option value=value prop:selected=move || target.get() == selected>{label}</option> }
            })
            .collect_view()
    };

    let duration_options =
        DURATIONS.iter().map(|&(value, label)| view! { <option value=value prop:selected=move || duration.get() == value>{label}</option> }).collect_view();

    let mute_item = move |mute: Mute| {
        let id = mute.id.clone();
        let reason = if mute.reason.is_empty() { String::new() } else { format!(", {}", mute.reason) };
        let meta = format!("Until {}{reason}", format::when(mute.until as f64, js_sys::Date::now()));
        view! {
            <li>
                {sev_icon(Severity::Muted)}
                <div>
                    <div>{mute.label}</div>
                    <div class="meta">{meta}</div>
                </div>
                <button class="btn" type="button" on:click=move |_| store.unmute(id.clone())>"Unmute"</button>
            </li>
        }
    };

    let history_item = move |event: Event| {
        let note = if event.note.is_empty() { String::new() } else { format!(", {}", event.note) };
        let meta = format!("{}, {}, {}{note}", format::day_label(event.time as f64, js_sys::Date::now()), format::hhmm(event.time as f64), event.kind);
        view! {
            <li>
                {sev_icon(event.severity)}
                <div>
                    <div><b>{event.target}</b>{event.text}</div>
                    <div class="meta">{meta}</div>
                </div>
            </li>
        }
    };

    view! {
        <section>
            <div class="sec-head">
                <h2>"Active"</h2>
                <span class="meta">"One notification per change, sent to your phone"</span>
            </div>
            <AlertList/>
        </section>
        <div class="two">
            <section>
                <div class="sec-head">
                    <h2>"Mutes"</h2>
                    <span class="meta">"Muted alerts stay visible but send nothing"</span>
                </div>
                <Show when=move || store.mutes().is_empty()>
                    <p class="meta">"No mutes right now."</p>
                </Show>
                <ul class="mutes">
                    <For each=move || store.mutes() key=|mute| mute.id.clone() children=mute_item/>
                </ul>
                <form class="mute-form" on:submit=submit>
                    <h3>"New mute"</h3>
                    <label for="mute-target">"What"</label>
                    <select id="mute-target" on:change=move |ev| target.set(event_target_value(&ev))>
                        {target_options}
                    </select>
                    <label for="mute-duration">"How long"</label>
                    <select id="mute-duration" node_ref=duration_select on:change=move |ev| duration.set(event_target_value(&ev))>
                        {duration_options}
                    </select>
                    <label for="mute-reason">"Reason"</label>
                    <input
                        id="mute-reason"
                        type="text"
                        maxlength="200"
                        placeholder="For example disk swap"
                        autocomplete="off"
                        prop:value=move || reason.get()
                        on:input=move |ev| reason.set(event_target_value(&ev))
                    />
                    <div class="form-act">
                        <button class="btn primary" type="submit" disabled=move || target.get().is_empty()>"Mute"</button>
                    </div>
                </form>
            </section>
            <section>
                <div class="sec-head">
                    <h2>"History"</h2>
                    <span class="meta">"Latest 100 changes"</span>
                </div>
                <Show when=move || store.events.with(Vec::is_empty)>
                    <p class="meta">"Nothing happened yet."</p>
                </Show>
                <ol class="history">
                    <For each=move || store.events.get() key=|event| event.id.clone() children=history_item/>
                </ol>
            </section>
        </div>
        <section>
            <div class="sec-head">
                <h2>"Rules"</h2>
                <span class="meta">"From the node config"</span>
            </div>
            <div class="tbl-wrap">
                <table>
                    <thead>
                        <tr><th>"Rule"</th><th>"Applies to"</th><th>"Condition"</th><th>"Severity"</th></tr>
                    </thead>
                    <tbody>
                        {move || rules.get().into_iter().map(|rule| view! {
                            <tr>
                                <td>{rule.name}</td>
                                <td>{rule.scope}</td>
                                <td class="mono">{rule.condition}</td>
                                <td><Pill severity=rule.severity/></td>
                            </tr>
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
        </section>
    }
}
