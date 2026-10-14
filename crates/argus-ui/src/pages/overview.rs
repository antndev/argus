use leptos::prelude::*;

use crate::components::{AlertList, BackupTable, HostCard, ServiceList};
use crate::store::use_store;

#[component]
pub fn OverviewPage() -> impl IntoView {
    let store = use_store();
    let ids = Memo::new(move |_| store.host_ids());
    view! {
        <section>
            <div class="sec-head">
                <h2>"Needs attention"</h2>
            </div>
            <AlertList/>
        </section>
        <section class="hosts">
            <div class="sec-head">
                <h2>"Hosts"</h2>
                <span class="meta">"Drag cards to change the order"</span>
            </div>
            <Show when=move || store.loaded() && ids.with(Vec::is_empty)>
                <p class="meta">"No agent has reported yet."</p>
            </Show>
            <div class="host-grid">
                <For
                    each=move || ids.get().into_iter().enumerate()
                    key=|(_, id)| id.clone()
                    children=|(index, id)| view! { <HostCard id=id index=index/> }
                />
            </div>
        </section>
        <section>
            <div class="sec-head">
                <h2>"Checks"</h2>
                <a class="link" href="#/checks">"All checks"</a>
            </div>
            <ServiceList/>
        </section>
        <section>
            <div class="sec-head">
                <h2>"ZeroByte backups"</h2>
                <span class="meta">"Schedules and S3 mirrors"</span>
            </div>
            <BackupTable/>
        </section>
    }
}
