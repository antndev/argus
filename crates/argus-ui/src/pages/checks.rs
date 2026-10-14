use std::collections::HashMap;

use argus_model::{CheckStatus, SeriesData, metric_key};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api;
use crate::components::{Pill, all_checks, segmented, uptime_strip};
use crate::format;
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
    let rows = move || {
        let filter = store.check_filter.get();
        checks
            .get()
            .into_iter()
            .filter(|row| filter == CheckFilter::All || row.waiting || row.check.status != CheckStatus::Ok)
            .map(|row| {
                let strip = history.with(|map| map.get(&(row.host.clone(), row.check.name.clone())).cloned());
                let runs = if row.standalone() {
                    let via = if row.runner.is_empty() { "no agent yet".to_string() } else { format!("via {}", row.runner) };
                    view! { <span>"Standalone"</span><span class="sub">{via}</span> }.into_any()
                } else {
                    view! { <a class="link" href=crate::route::Route::Host(row.host.clone()).href()>{row.host.clone()}</a> }.into_any()
                };
                let severity = row.severity();
                let check = row.check;
                view! {
                    <tr>
                        <td><Pill severity=severity/></td>
                        <td><span class="check-name">{check.name.clone()}</span><span class="sub mono clip" title=check.target.clone()>{check.target.clone()}</span></td>
                        <td class="hide-sm"><span class="tag">{check.kind.clone()}</span></td>
                        <td>{runs}</td>
                        <td><span class="msg" title=check.message.clone()>{check.message.clone()}</span></td>
                        <td>{uptime_strip(strip)}</td>
                        <td class="num">{check.latency_ms.map(format::latency).unwrap_or_default()}</td>
                    </tr>
                }
            })
            .collect_view()
    };
    let empty = move || store.loaded() && checks.with(Vec::is_empty);
    view! {
        <div class="filters">
            {segmented(store.check_filter, CheckFilter::OPTIONS, "Filter checks")}
            <a class="btn" href="#/settings/checks"><span class="ico" inner_html=crate::icons::PLUS></span>"Add service"</a>
        </div>
        <Show when=empty>
            <p class="meta">"No services yet."</p>
        </Show>
        <div class="tbl-wrap stack">
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
    }
}
