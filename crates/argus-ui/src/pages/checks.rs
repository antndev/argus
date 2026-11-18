use std::collections::HashMap;

use argus_model::{CheckStatus, SeriesData, metric_key};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{NO_SERVICES, all_checks, segmented, service_row, uptime_strip};
use crate::store::{CheckFilter, use_store};

#[component]
pub fn ChecksPage() -> impl IntoView {
    let store = use_store();
    let history = RwSignal::new(HashMap::<(String, String), Vec<Option<f64>>>::new());
    let checks = Memo::new(move |_| all_checks(store));
    let names = Memo::new(move |_| {
        let mut by_host: Vec<(String, Vec<String>)> = Vec::new();
        for row in checks.get() {
            match by_host.iter_mut().find(|(h, _)| *h == row.host) {
                Some((_, list)) => list.push(row.check.name),
                None => by_host.push((row.host, vec![row.check.name])),
            }
        }
        by_host
    });
    let period = Memo::new(move |_| (store.clock.get() / 300_000.0).floor() as i64);
    Effect::new(move |_| {
        period.track();
        for (host, list) in names.get() {
            for chunk in list.chunks(16) {
                let keys = chunk.iter().map(|name| metric_key("check", name, "ok")).collect::<Vec<_>>().join(",");
                let url = format!("/api/v1/series?host={}&keys={}&range=86400", api::encode(&host), api::encode(&keys));
                let chunk = chunk.to_vec();
                let host = host.clone();
                spawn_local(async move {
                    if let Ok(data) = api::get::<SeriesData>(&url).await {
                        let _ = history.try_update(|map| {
                            for (name, values) in chunk.into_iter().zip(data.values) {
                                map.insert((host.clone(), name), values);
                            }
                        });
                    }
                });
            }
        }
    });
    let shown = Memo::new(move |_| {
        let filter = store.check_filter.get();
        checks.get().into_iter().filter(|row| filter == CheckFilter::All || row.waiting || row.check.status != CheckStatus::Ok).collect::<Vec<_>>()
    });
    let rows = move || {
        shown
            .get()
            .into_iter()
            .map(|row| {
                let strip = history.with(|map| map.get(&(row.host.clone(), row.check.name.clone())).cloned());
                let runs = if row.standalone() {
                    let via = if row.runner.is_empty() { "No agent yet".to_string() } else { format!("Run by {}", row.runner) };
                    view! { <span>"Standalone"</span><span class="sub">{via}</span> }.into_any()
                } else {
                    view! { <a class="link" href=crate::route::Route::Host(row.host.clone()).href()>{row.host.clone()}</a> }.into_any()
                };
                service_row(row.severity(), row.check, Some(runs), Some(uptime_strip(strip)))
            })
            .collect_view()
    };
    let empty = move || if checks.with(Vec::is_empty) { NO_SERVICES } else { "All services are OK." };
    view! {
        <div class="filters">
            {segmented(store.check_filter, CheckFilter::OPTIONS, "Filter checks")}
            <a class="btn" href="#/settings/checks"><span class="ico" inner_html=crate::icons::PLUS></span>"Add service"</a>
        </div>
        <Show when=move || !shown.with(Vec::is_empty) fallback=move || store.loaded().then(|| view! { <p class="empty-note">{empty}</p> })>
            <div class="tbl-wrap sheet stack">
                <table>
                    <thead>
                        <tr>
                            <th>"Status"</th>
                            <th>"Service"</th>
                            <th>"Type"</th>
                            <th>"Runs on"</th>
                            <th>"Last result"</th>
                            <th>"Last 24 h"</th>
                            <th class="num">"Latency"</th>
                        </tr>
                    </thead>
                    <tbody>{rows}</tbody>
                </table>
            </div>
        </Show>
    }
}
