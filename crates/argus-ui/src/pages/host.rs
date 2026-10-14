use argus_model::HostSummary;
use leptos::prelude::*;

use crate::charts::{Chart, ChartSpec, Unit};
use crate::components::{AlertList, Pill, check_severity, segmented};
use crate::format;
use crate::icons;
use crate::store::{Range, key, latest, limit, temperatures, use_store};

fn threshold(spec: ChartSpec, value: Option<f64>, text: impl Fn(f64) -> String) -> ChartSpec {
    match value {
        Some(value) => spec.threshold(value, text(value)),
        None => spec,
    }
}

fn interfaces(host: &HostSummary) -> Vec<(String, String)> {
    let mut list: Vec<(String, String)> = host
        .latest
        .iter()
        .filter(|m| m.group == "net" && m.field == "rx")
        .map(|m| {
            let description = host.inventory.interfaces.iter().find(|i| i.name == m.item).and_then(|i| i.description.clone());
            (m.item.clone(), description.map_or_else(|| m.item.clone(), |d| format!("{d} ({})", m.item)))
        })
        .collect();
    list.sort_by_key(|(_, label)| !label.to_ascii_uppercase().starts_with("WAN"));
    list
}

fn chart_specs(host: &HostSummary) -> Vec<ChartSpec> {
    let id = host.info.id.as_str();
    let mut cpu = ChartSpec::new(id, "CPU", "Percent of all cores", Unit::Percent).max(100.0).series("Total", key("cpu", "", "total"));
    if latest(host, "cpu", "", "iowait").is_some() {
        cpu = cpu.series("I/O wait", key("cpu", "", "iowait"));
    }
    let cpu = threshold(cpu, limit(host, "cpu", "", "total"), |v| format!("Alert at {v}%"));
    let memory = threshold(
        ChartSpec::new(id, "Memory", "Percent of RAM", Unit::Percent)
            .max(100.0)
            .series("Used", key("memory", "", "used"))
            .series("Cache", key("memory", "", "cache")),
        limit(host, "memory", "", "used"),
        |v| format!("Alert at {v}%"),
    );
    let load_note = match limit(host, "load", "", "5") {
        Some(value) => format!("{} cores, alert above {value}", host.info.cores),
        None => format!("{} cores", host.info.cores),
    };
    let load = ChartSpec::new(id, "Load", load_note, Unit::Load)
        .floor(1.0)
        .series("1 min", key("load", "", "1"))
        .series("5 min", key("load", "", "5"))
        .series("15 min", key("load", "", "15"));
    let mut specs = vec![cpu, memory, load];
    let sensors: Vec<&argus_model::Metric> = temperatures(host).into_iter().take(3).collect();
    if !sensors.is_empty() {
        let mut spec = ChartSpec::new(id, "Temperatures", "Per sensor", Unit::Celsius);
        let mut seen: Vec<f64> = Vec::new();
        for sensor in &sensors {
            spec = spec.series(&sensor.item, sensor.key());
            if let Some(value) = limit(host, &sensor.group, &sensor.item, &sensor.field).filter(|v| !seen.contains(v)) {
                seen.push(value);
                let group = host.inventory.sensors.iter().find(|s| s.name == sensor.item).map_or(sensor.item.clone(), |s| s.group.clone());
                spec = spec.threshold(value, format!("{group} alert at {value} °C"));
            }
        }
        specs.push(spec);
    }
    for (name, label) in interfaces(host) {
        specs.push(ChartSpec::new(id, "Network", label, Unit::Rate).series("In", key("net", &name, "rx")).series("Out", key("net", &name, "tx")));
    }
    specs
}

fn bytes(value: u64) -> String {
    let gigabytes = value as f64 / 1e9;
    if gigabytes >= 1_000.0 {
        format!("{:.1} TB", gigabytes / 1_000.0)
    } else if gigabytes >= 10.0 {
        format!("{gigabytes:.0} GB")
    } else {
        format!("{gigabytes:.1} GB")
    }
}

fn facts(host: &HostSummary, now: f64) -> Vec<(String, String)> {
    let info = &host.info;
    let uptime = info.uptime_s as f64 * 1_000.0 + (now - host.last_seen as f64).max(0.0);
    let mut list = vec![
        ("Platform".to_string(), format!("{}, {}", info.os, info.arch)),
        ("Kernel".to_string(), info.kernel.clone()),
        ("Hardware".to_string(), format!("{} cores, {} RAM", info.cores, bytes(info.memory_bytes))),
        ("Uptime".to_string(), format::duration(uptime)),
        ("Agent".to_string(), format!("{} {}", format::capitalize(&info.agent), info.version)),
        ("Alerts from".to_string(), format!("{} node", host.owner)),
    ];
    for raid in &host.inventory.raid {
        list.push((format!("RAID {}", raid.name), format!("{}, {} {}", raid.level, raid.state, raid.devices)));
    }
    for disk in &host.inventory.smart {
        let mut text = if disk.passed { "Passed".to_string() } else { "Failing".to_string() };
        if let Some(used) = disk.percent_used {
            text.push_str(&format!(", {used:.0}% used"));
        }
        list.push((format!("SMART {}", disk.device), text));
    }
    if let Some(units) = &host.inventory.failed_units {
        list.push(("Failed units".to_string(), if units.is_empty() { "None".to_string() } else { units.join(", ") }));
    }
    if let Some(reboot) = host.inventory.reboot_required {
        list.push(("Reboot required".to_string(), if reboot { "Yes".to_string() } else { "No".to_string() }));
    }
    list.retain(|(_, value)| !value.trim().is_empty());
    list
}

#[component]
pub fn HostPage(host_id: String) -> impl IntoView {
    let store = use_store();
    let lookup = host_id.clone();
    let host = Memo::new(move |_| store.host(&lookup));
    let specs = Memo::new(move |_| host.with(|h| h.as_ref().map(chart_specs).unwrap_or_default()));
    let id = host_id.clone();
    let state = move || host.with(|h| h.as_ref().map(|h| h.state).unwrap_or_default());
    let seen = move || {
        host.with(|h| h.as_ref().filter(|h| h.last_seen > 0).map(|h| format!("Last report {}", format::ago(h.last_seen as f64, store.clock.get()))))
            .unwrap_or_default()
    };
    let about = move || {
        host.with(|h| {
            h.as_ref()
                .map(|h| [h.info.role.clone(), h.info.os.clone(), h.info.arch.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "))
        })
        .unwrap_or_default()
    };
    let filesystems = move || {
        host.with(|h| {
            h.as_ref()
                .map(|h| {
                    h.inventory
                        .filesystems
                        .iter()
                        .map(|fs| {
                            let used = latest(h, "fs", &fs.mount, "used").unwrap_or_else(|| fs.used_bytes as f64 / fs.size_bytes.max(1) as f64 * 100.0);
                            let alert = limit(h, "fs", &fs.mount, "used").unwrap_or(f64::MAX);
                            let over = used >= alert;
                            view! {
                                <li>
                                    <div class="fs-top">
                                        <span class="mono">{fs.mount.clone()}</span>
                                        <span>{format::percent(used)}" "<span class="dim">{format!("of {}", bytes(fs.size_bytes))}</span></span>
                                    </div>
                                    <span class=if over { "bar big warning" } else { "bar big" }>
                                        <i style=format!("width:{:.1}%", used.clamp(1.0, 100.0))></i>
                                    </span>
                                    {over.then(|| view! {
                                        <div class="fs-warn">
                                            <span class="ico" inner_html=icons::WARNING></span>
                                            {format!("Above the {alert}% limit")}
                                        </div>
                                    })}
                                </li>
                            }
                        })
                        .collect_view()
                })
                .into_iter()
                .collect_view()
        })
    };
    let fact_list = move || {
        host.with(|h| h.as_ref().map(|h| facts(h, store.clock.get())).unwrap_or_default())
            .into_iter()
            .map(|(name, value)| view! { <div><dt>{name}</dt><dd>{value}</dd></div> })
            .collect_view()
    };
    let containers = move || host.with(|h| h.as_ref().map(|h| h.inventory.containers.clone()).unwrap_or_default());
    let interfaces = move || {
        host.with(|h| {
            h.as_ref()
                .map(|h| {
                    h.inventory
                        .interfaces
                        .iter()
                        .map(|i| {
                            let rx = latest(h, "net", &i.name, "rx").map(format::rate).unwrap_or_default();
                            let tx = latest(h, "net", &i.name, "tx").map(format::rate).unwrap_or_default();
                            (i.description.clone().unwrap_or_else(|| i.name.clone()), i.name.clone(), rx, tx)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    let checks = move || host.with(|h| h.as_ref().map(|h| h.checks.clone()).unwrap_or_default());
    let many_interfaces = move || interfaces().len() > 1;

    view! {
        <div class="detail-head">
            <div>
                <h2>{id.clone()}</h2>
                <div class="detail-sub">
                    {move || view! { <Pill severity=state()/> }}
                    <span>{about}</span>
                    <span class="dim">{seen}</span>
                </div>
            </div>
            {segmented(store.range, Range::OPTIONS, "Time range")}
        </div>
        <Show when=move || store.loaded() && host.with(|h| h.as_ref().is_none_or(|h| h.last_seen == 0))>
            <p class="empty-note">"No report yet. Install the agent on this host with its token from Settings, Hosts and agents, it shows up here within 30 seconds."</p>
        </Show>
        <section>
            <AlertList host=id.clone()/>
        </section>
        <div class="chart-grid">{move || specs.get().into_iter().map(|spec| view! { <Chart spec=spec/> }).collect_view()}</div>
        <div class="two">
            <section>
                <div class="sec-head"><h2>"Filesystems"</h2></div>
                <ul class="fs">{filesystems}</ul>
            </section>
            <section>
                <div class="sec-head"><h2>"System"</h2></div>
                <dl class="facts">{fact_list}</dl>
            </section>
        </div>
        <Show when=move || !checks().is_empty()>
            <section>
                <div class="sec-head"><h2>"Checks"</h2><span class="meta">"Run by this agent"</span></div>
                <div class="tbl-wrap stack">
                    <table>
                        <thead><tr><th>"Status"</th><th>"Check"</th><th>"Type"</th><th>"Result"</th><th class="num">"Latency"</th></tr></thead>
                        <tbody>
                            {move || checks().into_iter().map(|c| view! {
                                <tr>
                                    <td><Pill severity=check_severity(c.status)/></td>
                                    <td><span class="check-name">{c.name.clone()}</span><span class="sub mono clip" title=c.target.clone()>{c.target.clone()}</span></td>
                                    <td class="hide-sm"><span class="tag">{c.kind.clone()}</span></td>
                                    <td><span class="msg" title=c.message.clone()>{c.message.clone()}</span></td>
                                    <td class="num">{c.latency_ms.map(format::latency).unwrap_or_default()}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            </section>
        </Show>
        <Show when=move || !containers().is_empty()>
            <section>
                <div class="sec-head"><h2>"Containers"</h2><span class="meta">"Docker through a read-only socket proxy"</span></div>
                <div class="tbl-wrap">
                    <table>
                        <thead>
                            <tr><th>"Container"</th><th>"State"</th><th class="num">"CPU"</th><th class="num">"Memory"</th><th class="num">"Restarts"</th><th>"Status"</th></tr>
                        </thead>
                        <tbody>
                            {move || containers().into_iter().map(|c| {
                                let running = c.state == "running";
                                view! {
                                    <tr>
                                        <td class="mono">{c.name.clone()}</td>
                                        <td><Pill severity=if running { argus_model::Severity::Ok } else { argus_model::Severity::Warning } text=format::capitalize(&c.state)/></td>
                                        <td class="num">{format!("{:.1}%", c.cpu)}</td>
                                        <td class="num">{format::memory(c.memory_bytes as f64 / 1_048_576.0)}</td>
                                        <td class="num">{c.restarts.to_string()}</td>
                                        <td class="dim">{c.status.clone()}</td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            </section>
        </Show>
        <Show when=many_interfaces>
            <section>
                <div class="sec-head"><h2>"Interfaces"</h2></div>
                <div class="tbl-wrap">
                    <table>
                        <thead><tr><th>"Interface"</th><th class="num">"In"</th><th class="num">"Out"</th></tr></thead>
                        <tbody>
                            {move || interfaces().into_iter().map(|(label, name, rx, tx)| view! {
                                <tr>
                                    <td>{label}<span class="sub mono">{name}</span></td>
                                    <td class="num">{rx}</td>
                                    <td class="num">{tx}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            </section>
        </Show>
    }
}
