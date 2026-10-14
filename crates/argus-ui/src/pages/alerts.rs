use argus_model::{Event, RuleInfo};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{AlertList, Pill, sev_icon};
use crate::format;
use crate::store::use_store;

#[component]
pub fn AlertsPage() -> impl IntoView {
    let store = use_store();
    let rules = RwSignal::new(Vec::<RuleInfo>::new());

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
        <section>
            <div class="sec-head">
                <h2>"History"</h2>
                <span class="meta">"Latest 100 changes"</span>
            </div>
            <Show when=move || store.events.with(Vec::is_empty)>
                <p class="meta">"Nothing happened yet."</p>
            </Show>
            <ol class="history sheet">
                <For each=move || store.events.get() key=|event| event.id.clone() children=history_item/>
            </ol>
        </section>
        <section>
            <div class="sec-head">
                <h2>"Rules"</h2>
                <a class="link" href="#/settings/rules">"Change rules"</a>
            </div>
            <div class="tbl-wrap sheet">
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
