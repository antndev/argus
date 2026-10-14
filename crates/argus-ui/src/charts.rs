use argus_model::SeriesData;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;

use crate::api;
use crate::components::segmented;
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

    #[wasm_bindgen(catch, js_namespace = argusCharts, js_name = isolate)]
    fn js_isolate(id: u32, index: usize) -> Result<String, JsValue>;
}

const VIEWS: &[(bool, &str)] = &[(false, "Containers"), (true, "Each container")];

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
    series: Vec<(String, String, f64)>,
    detail: Vec<(String, String, f64)>,
    max: Option<f64>,
    floor: f64,
    thresholds: Vec<(f64, String)>,
}

impl ChartSpec {
    pub fn new(host: &str, title: &str, note: impl Into<String>, unit: Unit) -> Self {
        Self {
            host: host.to_string(),
            title: title.to_string(),
            note: note.into(),
            unit,
            series: Vec::new(),
            detail: Vec::new(),
            max: None,
            floor: 0.0,
            thresholds: Vec::new(),
        }
    }

    pub fn series(self, label: &str, key: String) -> Self {
        self.scaled(label, key, 1.0)
    }

    pub fn scaled(mut self, label: &str, key: String, factor: f64) -> Self {
        self.series.push((label.to_string(), key, factor));
        self
    }

    pub fn detail(mut self, label: &str, key: String, factor: f64) -> Self {
        self.detail.push((label.to_string(), key, factor));
        self
    }

    fn active(&self, detailed: bool) -> Self {
        if !detailed || self.detail.is_empty() {
            return self.clone();
        }
        let mut series = self.series[..self.series.len().saturating_sub(1)].to_vec();
        series.extend(self.detail.iter().cloned());
        Self { series, ..self.clone() }
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
            "series": self.series.iter().map(|(label, _, _)| label).collect::<Vec<_>>(),
            "thresholds": thresholds,
        })
        .to_string()
    }

    fn url(&self, range: Range) -> String {
        let keys = self.series.iter().map(|(_, key, _)| key.as_str()).collect::<Vec<_>>().join(",");
        format!("/api/v1/series?host={}&keys={}&range={}", api::encode(&self.host), api::encode(&keys), range.seconds())
    }
}

#[component]
pub fn Chart(spec: ChartSpec) -> impl IntoView {
    let store = use_store();
    let node = NodeRef::<html::Div>::new();
    let handle = StoredValue::new(None::<u32>);
    let request = StoredValue::new(0u32);
    let ranged = StoredValue::new(None::<Range>);
    let latest = RwSignal::new(Vec::<Option<f64>>::new());
    let shown = RwSignal::new(Vec::<bool>::new());
    let tick = Memo::new(move |_| (store.clock.get() / 30_000.0).floor() as i64);
    let unit = spec.unit;
    let title = spec.title.clone();
    let note = spec.note.clone();
    let splits = !spec.detail.is_empty();
    let detailed = RwSignal::new(false);
    let mode = StoredValue::new(false);
    let spec = StoredValue::new(spec);
    let current = Memo::new(move |_| spec.with_value(|s| s.active(detailed.get())));

    Effect::new(move |_| {
        let range = store.range.get();
        tick.track();
        let Some(element) = node.get() else { return };
        let active = current.get();
        if mode.get_value() != detailed.get_untracked() {
            mode.set_value(detailed.get_untracked());
            if let Some(id) = handle.get_value() {
                let _ = js_destroy(id);
                handle.set_value(None);
            }
            shown.set(Vec::new());
        }
        let url = active.url(range);
        let json = active.to_json();
        let factors: Vec<f64> = active.series.iter().map(|(_, _, factor)| *factor).collect();
        let sequence = request.get_value() + 1;
        request.set_value(sequence);
        spawn_local(async move {
            let Ok(mut data) = api::get::<SeriesData>(&url).await else {
                return;
            };
            for (column, factor) in data.values.iter_mut().zip(&factors) {
                column.iter_mut().flatten().for_each(|value| *value *= factor);
            }
            if request.try_get_value() != Some(sequence) {
                return;
            }
            let values = data.values.iter().map(|column| column.iter().rev().find_map(|v| *v)).collect();
            latest.set(values);
            let payload = serde_json::to_string(&data).unwrap_or_default();
            let animate = ranged.get_value().is_some_and(|previous| previous != range);
            ranged.set_value(Some(range));
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

    let legend = move || {
        current
            .get()
            .series
            .into_iter()
            .enumerate()
            .map(|(index, (label, _, _))| {
                let value = move || latest.with(|v| v.get(index).copied().flatten().map(|x| unit.format(x)).unwrap_or_default());
                let off = move || shown.with(|s| s.get(index).is_some_and(|show| !show));
                let pick = move |_| {
                    if let Some(id) = handle.get_value()
                        && let Ok(json) = js_isolate(id, index)
                    {
                        shown.set(serde_json::from_str(&json).unwrap_or_default());
                    }
                };
                view! {
                    <button type="button" class:off=off on:click=pick title="Show only this, click again for all">
                        <i style=format!("background:var(--s{})", index % 8 + 1)></i>
                        {label}
                        <b>{value}</b>
                    </button>
                }
            })
            .collect_view()
    };

    view! {
        <section class="panel">
            <header class="panel-head">
                <h3>{title}</h3>
                {splits.then(|| segmented(detailed, VIEWS, "Containers"))}
                <span class="meta">{note}</span>
            </header>
            <div class="legend">{legend}</div>
            <div class="chart" node_ref=node></div>
        </section>
    }
}
