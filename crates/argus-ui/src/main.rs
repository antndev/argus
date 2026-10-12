mod api;
mod app;
mod charts;
mod components;
mod format;
mod icons;
mod pages;
mod route;
mod simulation;
mod store;
mod theme;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
