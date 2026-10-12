use leptos::prelude::{document, window};

const KEY: &str = "argus-theme";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    pub const OPTIONS: &'static [(Theme, &'static str)] = &[(Theme::System, "System"), (Theme::Light, "Light"), (Theme::Dark, "Dark")];

    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn load() -> Self {
        let stored = storage().and_then(|s| s.get_item(KEY).ok().flatten());
        match stored.as_deref() {
            Some("light") => Self::Light,
            Some("dark") => Self::Dark,
            _ => Self::System,
        }
    }

    pub fn apply(self) {
        if let Some(root) = document().document_element() {
            let _ = match self {
                Self::System => root.remove_attribute("data-theme"),
                other => root.set_attribute("data-theme", other.key()),
            };
        }
        if let Some(storage) = storage() {
            let _ = storage.set_item(KEY, self.key());
        }
    }
}

fn storage() -> Option<web_sys::Storage> {
    window().local_storage().ok().flatten()
}
