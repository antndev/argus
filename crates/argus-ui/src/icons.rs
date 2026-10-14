use argus_model::Severity;

pub const OK: &str = r#"<svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6.3" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M5.2 8.3l1.9 1.9 3.8-4.2" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>"#;

pub const WARNING: &str = r#"<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.1l6.4 11.4H1.6z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/><path d="M8 6.4v3.3" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/><circle cx="8" cy="11.7" r=".9" fill="currentColor"/></svg>"#;

pub const CRITICAL: &str = r#"<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5.4 1.8h5.2l3.6 3.6v5.2l-3.6 3.6H5.4l-3.6-3.6V5.4z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round"/><path d="M6 6l4 4M10 6l-4 4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>"#;

pub const LOGO: &str = r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path class="lid" d="M2 12c2.8-4.7 6.1-7 10-7s7.2 2.3 10 7c-2.8 4.7-6.1 7-10 7s-7.2-2.3-10-7z" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"/><g class="iris"><circle cx="12" cy="12" r="3.6" fill="currentColor"/><circle cx="13.2" cy="10.8" r="1" fill="var(--bg-side)"/></g></svg>"#;

macro_rules! line_icon {
    ($name:ident, $body:literal) => {
        pub const $name: &str = concat!(
            r#"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">"#,
            $body,
            "</svg>"
        );
    };
}

line_icon!(
    OVERVIEW,
    r#"<rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1"/><rect x="9" y="2.5" width="4.5" height="4.5" rx="1"/><rect x="2.5" y="9" width="4.5" height="4.5" rx="1"/><rect x="9" y="9" width="4.5" height="4.5" rx="1"/>"#
);
line_icon!(
    HOSTS,
    r#"<rect x="2.5" y="2.5" width="11" height="4.5" rx="1"/><rect x="2.5" y="9" width="11" height="4.5" rx="1"/><path d="M5 4.75h.01M5 11.25h.01"/>"#
);
line_icon!(CHECKS, r#"<path d="M1.5 8.5h2.75l1.75-4 3 7.5 1.75-3.5h3.75"/>"#);
line_icon!(ALERTS, r#"<path d="M4 11V7.25a4 4 0 0 1 8 0V11l1.25 1.5h-10.5zM6.75 14h2.5"/>"#);
line_icon!(
    SETTINGS,
    r#"<g transform="scale(0.6667)" stroke-width="2.25"><path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/></g>"#
);
line_icon!(SEARCH, r#"<circle cx="7" cy="7" r="4.5"/><path d="m10.5 10.5 3 3"/>"#);
line_icon!(WAITING, r#"<circle cx="8" cy="8" r="6"/><path d="M8 4.75V8l2.25 1.5"/>"#);
line_icon!(BACK, r#"<path d="M10 3.5 5.5 8l4.5 4.5"/>"#);
line_icon!(PLUS, r#"<path d="M8 3v10M3 8h10"/>"#);
line_icon!(TRASH, r#"<path d="M2.5 4.5h11M6 4.5V3h4v1.5M4 4.5l.7 9h6.6l.7-9"/>"#);
line_icon!(COPY, r#"<rect x="5.5" y="5.5" width="8" height="8" rx="1.25"/><path d="M3 10.5V3.75C3 3.3 3.3 3 3.75 3h6.75"/>"#);
line_icon!(REFRESH, r#"<path d="M13.25 8A5.25 5.25 0 1 1 11.7 4.3M13.25 2.5v3h-3"/>"#);

pub fn severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Ok => OK,
        Severity::Muted => WAITING,
        Severity::Warning => WARNING,
        Severity::Critical => CRITICAL,
    }
}
