use argus_model::Severity;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::{class, move_keys};
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
            let _ = window().location().replace(&Route::Host(first).href());
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
    let drag = RwSignal::new(None::<Drag>);
    let settling = RwSignal::new(false);
    let finish = move || {
        let Some(done) = drag.get_untracked() else { return };
        settling.set(true);
        if done.to != done.from
            && let Some(target) = ids.with_untracked(|list| list.get(done.to).cloned())
            && let Some(at) = store.host_ids().iter().position(|h| *h == target)
        {
            store.move_host(&done.id, at);
        }
        drag.set(None);
        request_animation_frame(move || request_animation_frame(move || settling.set(false)));
    };
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
        let (target, lifted, shifted, grabbed) = (id.clone(), id.clone(), id.clone(), id.clone());
        let current = move || (selected.get().as_deref() == Some(target.as_str())).then_some("page");
        let style = move || {
            drag.with(|d| match d {
                Some(d) if d.id == shifted => format!("transform:translateY({}px)", d.offset),
                Some(d) => {
                    let at = ids.with(|list| list.iter().position(|h| *h == shifted)).unwrap_or(d.from);
                    let shift = match () {
                        _ if d.to > d.from && at > d.from && at <= d.to => -d.step,
                        _ if d.to < d.from && at >= d.to && at < d.from => d.step,
                        _ => 0.0,
                    };
                    format!("transform:translateY({shift}px)")
                }
                None => String::new(),
            })
        };
        view! {
            <li data-host=id.clone() class:lifted=move || drag.with(|d| d.as_ref().is_some_and(|d| d.id == lifted)) style=style>
                <a class="list-item" href=Route::Host(id.clone()).href() aria-current=current aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown" on:keydown=move_keys(store, id.clone())>
                    <span class=move || format!("tile {}", class(state()))>
                        <span class="ico" inner_html=icons::HOSTS></span>
                    </span>
                    <span class="li-text">
                        <span class="li-title">{id.clone()}</span>
                        <span class="li-sub">{sub}</span>
                    </span>
                </a>
                <span
                    class="grip"
                    role="button"
                    tabindex="-1"
                    aria-label=format!("Move {id}")
                    inner_html=icons::GRIP
                    on:pointerdown=move |ev: leptos::ev::PointerEvent| {
                        ev.prevent_default();
                        let Some(handle) = ev.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else { return };
                        let _ = handle.set_pointer_capture(ev.pointer_id());
                        let Some(row) = handle.parent_element().and_then(|r| r.dyn_into::<web_sys::HtmlElement>().ok()) else { return };
                        let other = row.next_element_sibling().or_else(|| row.previous_element_sibling()).and_then(|o| o.dyn_into::<web_sys::HtmlElement>().ok());
                        let step = other.map_or(row.offset_height(), |other| (other.offset_top() - row.offset_top()).abs());
                        let Some(from) = ids.with_untracked(|list| list.iter().position(|h| *h == grabbed)) else { return };
                        drag.set(Some(Drag { id: grabbed.clone(), from, to: from, start: f64::from(ev.client_y()), offset: 0.0, step: f64::from(step.max(1)) }));
                    }
                    on:pointermove=move |ev: leptos::ev::PointerEvent| {
                        let count = ids.with_untracked(Vec::len);
                        drag.update(|d| {
                            if let Some(d) = d {
                                d.offset = f64::from(ev.client_y()) - d.start;
                                d.to = (d.from as f64 + (d.offset / d.step).round()).clamp(0.0, count.saturating_sub(1) as f64) as usize;
                            }
                        });
                    }
                    on:pointerup=move |_| finish()
                    on:pointercancel=move |_| finish()
                ></span>
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
                <ul class="list" class:settling=move || settling.get() class:sorting=move || drag.with(Option::is_some) aria-label="Hosts">
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

#[derive(Clone, Debug, PartialEq)]
struct Drag {
    id: String,
    from: usize,
    to: usize,
    start: f64,
    offset: f64,
    step: f64,
}
