use argus_model::Severity;
use leptos::prelude::*;

use crate::components::class;
use crate::format;
use crate::icons;
use crate::pages::HostPage;
use crate::route::Route;
use crate::store::use_store;

#[component]
pub fn HostsPage() -> impl IntoView {
    let store = use_store();
    let filter = RwSignal::new(String::new());
    let selected = Memo::new(move |_| match store.route.get() {
        Route::Host(id) => Some(id),
        _ => None,
    });
    Effect::new(move |_| {
        let compact = window().inner_width().ok().and_then(|w| w.as_f64()).is_some_and(|w| w <= 820.0);
        if !compact
            && selected.get().is_none()
            && let Some(first) = store.host_ids().into_iter().next()
        {
            Route::Host(first).go();
        }
    });
    let ids = Memo::new(move |_| {
        let query = filter.get().to_lowercase();
        store
            .hosts()
            .into_iter()
            .filter(|h| query.is_empty() || h.info.id.to_lowercase().contains(&query) || h.info.role.to_lowercase().contains(&query))
            .map(|h| h.info.id)
            .collect::<Vec<_>>()
    });
    let item = move |id: String| {
        let lookup = id.clone();
        let host = Memo::new(move |_| store.host(&lookup));
        let state = move || host.with(|h| h.as_ref().map(|h| h.state).unwrap_or_default());
        let sub = move || {
            host.with(|h| {
                h.as_ref()
                    .map(|h| {
                        if h.last_seen == 0 {
                            return "Waiting for the first report".to_string();
                        }
                        let role = if h.info.role.is_empty() { format::capitalize(&h.info.os) } else { h.info.role.clone() };
                        match h.state {
                            Severity::Critical => format!("{role}, needs attention"),
                            Severity::Warning => format!("{role}, warning"),
                            _ => format!("{role}, {}", format::ago(h.last_seen as f64, store.clock.get())),
                        }
                    })
                    .unwrap_or_default()
            })
        };
        let target = id.clone();
        let current = move || (selected.get().as_deref() == Some(target.as_str())).then_some("page");
        view! {
            <li>
                <a class="list-item" href=Route::Host(id.clone()).href() aria-current=current>
                    <span class=move || format!("tile {}", class(state()))>
                        <span class="ico" inner_html=icons::HOSTS></span>
                    </span>
                    <span class="li-text">
                        <span class="li-title">{id.clone()}</span>
                        <span class="li-sub">{sub}</span>
                    </span>
                </a>
            </li>
        }
    };
    view! {
        <div class="split" class:has-detail=move || selected.with(Option::is_some)>
            <div class="split-list">
                <a class="btn" href="#/settings/agents"><span class="ico" inner_html=icons::PLUS></span>"Add host"</a>
                <label class="filter">
                    <span class="ico" inner_html=icons::SEARCH></span>
                    <input type="search" placeholder="Filter hosts" aria-label="Filter hosts" prop:value=move || filter.get() on:input=move |ev| filter.set(event_target_value(&ev))/>
                </label>
                <ul class="list" aria-label="Hosts">
                    <For each=move || ids.get() key=|id| id.clone() children=item/>
                </ul>
                <Show when=move || store.loaded() && ids.with(Vec::is_empty)>
                    <p class="empty-note">{move || if filter.with(String::is_empty) { "No agent has reported yet. Add one under Settings, Hosts and agents." } else { "No host matches." }}</p>
                </Show>
            </div>
            <section class="split-detail wide" aria-label="Host">
                <a class="back" href="#/hosts"><span class="ico" inner_html=icons::BACK></span>"Hosts"</a>
                {move || selected.get().map(|id| view! { <div class="page"><HostPage host_id=id/></div> })}
            </section>
        </div>
    }
}
