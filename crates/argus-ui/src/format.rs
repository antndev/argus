use wasm_bindgen::JsValue;

const S: f64 = 1_000.0;
const DAY: f64 = 86_400.0 * S;

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn date(t: f64) -> js_sys::Date {
    js_sys::Date::new(&JsValue::from_f64(t))
}

fn same_day(a: &js_sys::Date, b: &js_sys::Date) -> bool {
    a.get_full_year() == b.get_full_year() && a.get_month() == b.get_month() && a.get_date() == b.get_date()
}

pub fn hhmm(t: f64) -> String {
    let d = date(t);
    format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
}

pub fn day_label(t: f64, now: f64) -> String {
    let d = date(t);
    if same_day(&d, &date(now)) {
        "Today".to_string()
    } else if same_day(&d, &date(now - DAY)) {
        "Yesterday".to_string()
    } else {
        format!("{} {}", d.get_date(), MONTHS[d.get_month() as usize])
    }
}

pub fn when(t: f64, now: f64) -> String {
    let d = date(t);
    if same_day(&d, &date(now)) {
        hhmm(t)
    } else if same_day(&d, &date(now + DAY)) {
        format!("tomorrow {}", hhmm(t))
    } else {
        format!("{} {}", WEEKDAYS[d.get_day() as usize], hhmm(t))
    }
}

pub fn duration(ms: f64) -> String {
    let seconds = (ms / S).round().max(1.0) as u64;
    if seconds < 60 {
        return format!("{seconds} s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes} min");
    }
    let (hours, rest) = (minutes / 60, minutes % 60);
    if hours < 24 {
        return if rest > 0 { format!("{hours} h {rest} min") } else { format!("{hours} h") };
    }
    let (days, rest) = (hours / 24, hours % 24);
    if rest > 0 { format!("{days} d {rest} h") } else { format!("{days} d") }
}

pub fn ago(t: f64, now: f64) -> String {
    format!("{} ago", duration(now - t))
}

pub fn percent(value: f64) -> String {
    format!("{}%", value.round())
}

pub fn celsius(value: f64) -> String {
    format!("{} °C", value.round())
}

pub fn rate(value: f64) -> String {
    if value < 1.0 {
        format!("{} kbit/s", (value * 1_000.0).round())
    } else if value < 100.0 {
        format!("{value:.1} Mbit/s")
    } else {
        format!("{} Mbit/s", value.round())
    }
}

pub fn latency(ms: f64) -> String {
    if ms < 10.0 { format!("{ms:.1} ms") } else { format!("{} ms", ms.round()) }
}

pub fn memory(megabytes: f64) -> String {
    if megabytes >= 1_000.0 { format!("{:.1} GB", megabytes / 1_024.0) } else { format!("{megabytes:.0} MB") }
}

pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}
