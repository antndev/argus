use leptos::prelude::window;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Route {
    Overview,
    Hosts,
    Host(String),
    Checks,
    Alerts,
    Settings(String),
}

impl Route {
    pub fn current() -> Self {
        Self::parse(&window().location().hash().unwrap_or_default())
    }

    pub fn parse(hash: &str) -> Self {
        let path = hash.trim_start_matches('#').trim_matches('/');
        let mut parts = path.splitn(2, '/');
        let decode = |text: &str| js_sys::decode_uri_component(text).map(String::from).unwrap_or_else(|_| text.to_string());
        match (parts.next(), parts.next()) {
            (Some("hosts"), Some(id)) if !id.is_empty() => Self::Host(decode(id)),
            (Some("hosts"), None) => Self::Hosts,
            (Some("checks"), None) => Self::Checks,
            (Some("alerts"), None) => Self::Alerts,
            (Some("settings"), page) => Self::Settings(page.map(decode).unwrap_or_default()),
            _ => Self::Overview,
        }
    }

    pub fn href(&self) -> String {
        match self {
            Self::Overview => "#/".to_string(),
            Self::Hosts => "#/hosts".to_string(),
            Self::Host(id) => format!("#/hosts/{}", js_sys::encode_uri_component(id)),
            Self::Checks => "#/checks".to_string(),
            Self::Alerts => "#/alerts".to_string(),
            Self::Settings(page) if page.is_empty() => "#/settings".to_string(),
            Self::Settings(page) => format!("#/settings/{}", js_sys::encode_uri_component(page)),
        }
    }

    pub fn go(&self) {
        let _ = window().location().set_hash(self.href().trim_start_matches('#'));
    }

    pub fn section(&self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Hosts | Self::Host(_) => "hosts",
            Self::Checks => "checks",
            Self::Alerts => "alerts",
            Self::Settings(_) => "settings",
        }
    }
}
