mod app;
mod data;
mod fft;

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("scope_analyzer=info,warn")
        .with_target(false)
        .init();

    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Scope Analyzer")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([980.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Scope Analyzer",
        native_options,
        Box::new(|cc| Box::new(app::ScopeApp::new(cc))),
    )
}
