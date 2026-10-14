use argus_model::SeriesData;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;

use crate::api;
use crate::format;
use crate::store::{Range, use_store};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = argusCharts, js_name = create)]
    fn js_create(element: &web_sys::HtmlElement, spec: &str, data: &str) -> Result<u32, JsValue>;

    #[wasm_bindgen(catch, js_namespace = argusCharts, js_name = update)]
    fn js_update(id: u32, data: &str, animate: bool) -> Result<(), JsValue>;

    #[wasm_bindgen(catch, js_namespace = argusCharts, js_name = destroy)]
    fn js_destroy(id: u32) -> Result<(), JsValue>;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    Percent,
    Celsius,
    Rate,
    Load,
}

impl Unit {
    fn key(self) -> &'static str {
        match self {
            Self::Percent => "pct",
            Self::Celsius => "deg",
            Self::Rate => "rate",
            Self::Load => "load",
        }
    }

    fn format(self, value: f64) -> String {
        match self {
            Self::Percent => format!("{value:.1}%"),
            Self::Celsius => format!("{value:.1} °C"),
            Self::Rate => format::rate(value),
            Self::Load => format!("{value:.2}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChartSpec {
    host: String,
    title: String,
    note: String,
    unit: Unit,
    series: Vec<(String, String)>,
    max: Option<f64>,
    floor: f64,
    thresholds: Vec<(f64, String)>,
}

impl ChartSpec {
    pub fn new(host: &str, title: &str, note: impl Into<String>, unit: Unit) -> Self {
        Self { host: host.to_string(), title: title.to_string(), note: note.into(), unit, series: Vec::new(), max: None, floor: 0.0, thresholds: Vec::new() }
    }

    pub fn series(mut self, label: &str, key: String) -> Self {
        self.series.push((label.to_string(), key));
        self
    }

    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }

    pub fn floor(mut self, floor: f64) -> Self {
        self.floor = floor;
        self
    }

    pub fn threshold(mut self, value: f64, label: impl Into<String>) -> Self {
        self.thresholds.push((value, label.into()));
        self
    }

    fn to_json(&self) -> String {
        let thresholds: Vec<serde_json::Value> = self.thresholds.iter().map(|(v, label)| serde_json::json!({ "v": v, "label": label })).collect();
        serde_json::json!({
            "unit": self.unit.key(),
            "max": self.max,
            "floor": self.floor,
            "series": self.series.iter().map(|(label, _)| label).collect::<Vec<_>>(),
            "thresholds": thresholds,
        })
        .to_string()
    }

    fn url(&self, range: Range) -> String {
        let keys = self.series.iter().map(|(_, key)| key.as_str()).collect::<Vec<_>>().join(",");
        format!("/api/v1/series?host={}&keys={}&range={}", api::encode(&self.host), api::encode(&keys), range.seconds())
    }
}

#[component]
pub fn Chart(spec: ChartSpec) -> impl IntoView {
    let store = use_store();
    let node = NodeRef::<html::Div>::new();
    let handle = StoredValue::new(None::<u32>);
    let request = StoredValue::new(0u32);
    let shown = StoredValue::new(None::<Range>);
    let latest = RwSignal::new(Vec::<Option<f64>>::new());
    let tick = Memo::new(move |_| (store.clock.get() / 30_000.0).floor() as i64);
    let json = spec.to_json();
    let unit = spec.unit;
    let title = spec.title.clone();
    let note = spec.note.clone();
    let labels: Vec<String> = spec.series.iter().map(|(label, _)| label.clone()).collect();
    let spec = StoredValue::new(spec);

    Effect::new(move |_| {
        let range = store.range.get();
        tick.track();
        let Some(element) = node.get() else { return };
        let url = spec.with_value(|s| s.url(range));
        let json = json.clone();
        let sequence = request.get_value() + 1;
        request.set_value(sequence);
        spawn_local(async move {
            let Ok(data) = api::get::<SeriesData>(&url).await else {
                return;
            };
            if request.try_get_value() != Some(sequence) {
                return;
            }
            let values = data.values.iter().map(|column| column.iter().rev().find_map(|v| *v)).collect();
            latest.set(values);
            let payload = serde_json::to_string(&data).unwrap_or_default();
            let animate = shown.get_value().is_some_and(|previous| previous != range);
            shown.set_value(Some(range));
            match handle.get_value() {
                Some(id) => {
                    let _ = js_update(id, &payload, animate);
                }
                None => {
                    if let Ok(id) = js_create(&element, &json, &payload) {
                        handle.set_value(Some(id));
                    }
                }
            }
        });
    });

    on_cleanup(move || {
        if let Some(id) = handle.try_get_value().flatten() {
            let _ = js_destroy(id);
        }
    });

    let legend = labels
        .into_iter()
        .enumerate()
        .map(|(index, label)| {
            let value = move || latest.with(|v| v.get(index).copied().flatten().map(|x| unit.format(x)).unwrap_or_default());
            view! {
                <span>
                    <i style=format!("background:var(--s{})", index + 1)></i>
                    {label}
                    <b>{value}</b>
                </span>
            }
        })
        .collect_view();

    view! {
        <section class="panel">
            <header class="panel-head">
                <h3>{title}</h3>
                <span class="meta">{note}</span>
            </header>
            <div class="legend">{legend}</div>
            <div class="chart" node_ref=node></div>
        </section>
    }
}
