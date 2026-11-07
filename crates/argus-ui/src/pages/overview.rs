use leptos::prelude::*;

use crate::charts::{Chart, ChartSpec, Unit};
use crate::components::{AlertList, BackupTable, HostCard, ServiceList};
use crate::store::{Range, use_store};

#[component]
pub fn OverviewPage() -> impl IntoView {
    let store = use_store();
    let ids = Memo::new(move |_| store.host_ids());
    let sized = Memo::new(move |_| {
        let mut names: Vec<String> =
            store.overview.with(|o| o.as_ref().map(|o| o.backups.iter().filter(|b| !b.size.is_empty()).map(|b| b.name.clone()).collect()).unwrap_or_default());
        names.sort();
        names
    });
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
                <h2>"Services"</h2>
            </div>
            <ServiceList/>
        </section>
        <section>
            <div class="sec-head">
                <h2>"ZeroByte backups"</h2>
            </div>
            <BackupTable/>
            {move || {
                sized.with(|sized| (!sized.is_empty()).then(|| {
                    let spec = sized.iter().fold(ChartSpec::new(argus_model::BACKUP_HOST, "Backup size", Unit::Size).range(Range::Month), |spec, name| {
                        spec.series(name, argus_model::metric_key("backup", name, "size"))
                    });
                    view! { <div class="backup-chart"><Chart spec=spec/></div> }
                }))
            }}
        </section>
    }
}
