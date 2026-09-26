#[cfg(target_os = "linux")]
mod ui;
#[cfg(target_os = "linux")]
mod worker;

#[cfg(target_os = "linux")]
fn main() -> gtk::glib::ExitCode {
    use adw::prelude::*;
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let app = adw::Application::builder()
        .application_id("app.waveflow.WaveFlow")
        .build();
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
        } else {
            ui::build(app);
        }
    });
    app.run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("The native GTK frontend is available on Linux only.");
    std::process::exit(1);
}
