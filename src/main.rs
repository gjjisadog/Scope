mod app;
mod data;
mod fft;
mod png_export;
mod svg_export;
mod transforms;
mod vscode_bridge;

#[cfg(test)]
mod perf_tests;

use std::{
    fs::OpenOptions,
    io::Write as _,
    process::{Command, ExitStatus},
    time::{Duration, SystemTime},
};

const RENDERER_CHILD_ENV: &str = "SCOPE_RENDERER_CHILD";
const RENDERER_ENV: &str = "SCOPE_RENDERER";
const FALLBACK_WAIT: Duration = Duration::from_secs(6);
const INITIAL_WINDOW_SIZE: [f32; 2] = [1280.0, 760.0];
const MIN_WINDOW_SIZE: [f32; 2] = [860.0, 520.0];
const DEFAULT_RENDERER_ORDER: [RendererMode; 4] = [
    RendererMode::GlowSoftware,
    RendererMode::GlowHardware,
    RendererMode::WgpuDx12Software,
    RendererMode::WgpuDx12,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RendererMode {
    GlowHardware,
    GlowSoftware,
    WgpuDx12,
    WgpuDx12Software,
}

impl RendererMode {
    fn from_env_value(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "glow" | "opengl" => Some(Self::GlowHardware),
            "glow-software" | "opengl-software" | "software" | "cloud" | "virtual" => {
                Some(Self::GlowSoftware)
            }
            "wgpu" | "dx12" => Some(Self::WgpuDx12),
            "wgpu-software" | "dx12-software" | "warp" => Some(Self::WgpuDx12Software),
            _ => None,
        }
    }

    fn env_value(self) -> &'static str {
        match self {
            Self::GlowHardware => "glow",
            Self::GlowSoftware => "glow-software",
            Self::WgpuDx12 => "wgpu",
            Self::WgpuDx12Software => "wgpu-software",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::GlowHardware => "glow/OpenGL hardware",
            Self::GlowSoftware => "glow/OpenGL software",
            Self::WgpuDx12 => "wgpu/DX12",
            Self::WgpuDx12Software => "wgpu/DX12 software/WARP",
        }
    }

    fn renderer(self) -> eframe::Renderer {
        match self {
            Self::GlowHardware | Self::GlowSoftware => eframe::Renderer::Glow,
            Self::WgpuDx12 | Self::WgpuDx12Software => eframe::Renderer::Wgpu,
        }
    }

    fn hardware_acceleration(self) -> eframe::HardwareAcceleration {
        match self {
            Self::GlowSoftware | Self::WgpuDx12Software => eframe::HardwareAcceleration::Off,
            Self::GlowHardware | Self::WgpuDx12 => eframe::HardwareAcceleration::Preferred,
        }
    }

    fn force_wgpu_fallback_adapter(self) -> bool {
        matches!(self, Self::WgpuDx12Software)
    }
}

fn main() -> eframe::Result<()> {
    if let Some(exit_code) = vscode_bridge::run_from_args(std::env::args_os().skip(1)) {
        std::process::exit(exit_code);
    }

    configure_graphics_runtime();

    tracing_subscriber::fmt()
        .with_env_filter("scope_analyzer=info,warn")
        .with_target(false)
        .init();

    if let Ok(renderer) = std::env::var(RENDERER_ENV) {
        if let Some(mode) = RendererMode::from_env_value(&renderer) {
            write_startup_log(&format!("starting renderer: {}", mode.label()));
            return run_selected_renderer(mode);
        }
        write_startup_log(&format!(
            "unknown {RENDERER_ENV}={renderer}; using automatic fallback"
        ));
    }

    if std::env::var_os(RENDERER_CHILD_ENV).is_none() {
        return launch_with_process_fallback();
    }

    match try_run_app(RendererMode::GlowHardware) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            tracing::warn!("Glow/OpenGL renderer failed, retrying with software fallback: {error}");
            write_startup_log(&format!("in-process glow/OpenGL failed: {error}"));
            run_app(RendererMode::GlowSoftware)
        }
        Err(_) => {
            tracing::warn!(
                "Glow/OpenGL renderer panicked during startup, retrying software fallback"
            );
            write_startup_log("in-process glow/OpenGL panicked");
            run_app(RendererMode::GlowSoftware)
        }
    }
}

fn launch_with_process_fallback() -> eframe::Result<()> {
    for mode in DEFAULT_RENDERER_ORDER {
        write_startup_log(&format!("launcher: starting {} child", mode.label()));
        match spawn_renderer_child(mode) {
            Ok(mut child) => {
                let started = SystemTime::now();
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            write_startup_log(&format!(
                                "launcher: {} child exited early: {}",
                                mode.label(),
                                status_text(status)
                            ));
                            if status.success() {
                                return Ok(());
                            }
                            break;
                        }
                        Ok(None) => {
                            if started.elapsed().unwrap_or_default() >= FALLBACK_WAIT {
                                write_startup_log(&format!(
                                    "launcher: {} child is still running; startup accepted",
                                    mode.label()
                                ));
                                return Ok(());
                            }
                            std::thread::sleep(Duration::from_millis(150));
                        }
                        Err(error) => {
                            write_startup_log(&format!(
                                "launcher: failed to monitor {} child: {error}",
                                mode.label()
                            ));
                            return Ok(());
                        }
                    }
                }
            }
            Err(error) => {
                write_startup_log(&format!(
                    "launcher: failed to start {} child: {error}",
                    mode.label()
                ));
            }
        }
    }
    write_startup_log(exhausted_renderer_message());
    eprintln!("{}", exhausted_renderer_message());
    std::process::exit(1);
}

fn spawn_renderer_child(mode: RendererMode) -> std::io::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .env(RENDERER_CHILD_ENV, "1")
        .env(RENDERER_ENV, mode.env_value())
        .spawn()
}

fn status_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "terminated by system".to_owned(),
    }
}

fn try_run_app(mode: RendererMode) -> std::thread::Result<eframe::Result<()>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_app(mode)))
}

fn run_selected_renderer(mode: RendererMode) -> eframe::Result<()> {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|panic_info| {
        write_startup_log(&format!("renderer startup panic: {panic_info}"));
    }));

    let result = try_run_app(mode);
    std::panic::set_hook(previous_hook);

    match result {
        Ok(result) => result,
        Err(_) => {
            write_startup_log(&format!(
                "{} renderer panicked; exiting child so launcher can try the next renderer",
                mode.label()
            ));
            std::process::exit(101);
        }
    }
}

fn exhausted_renderer_message() -> &'static str {
    "launcher: all child renderers failed; not running WGPU/WARP in-process because virtual graphics drivers can panic before returning an error"
}

fn run_app(mode: RendererMode) -> eframe::Result<()> {
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    wgpu_options.supported_backends = eframe::wgpu::Backends::DX12;
    wgpu_options.power_preference = eframe::wgpu::PowerPreference::LowPower;
    wgpu_options.force_fallback_adapter = mode.force_wgpu_fallback_adapter();

    let mut viewport = base_scope_viewport_builder();
    if let Some(icon) = scope_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        renderer: mode.renderer(),
        hardware_acceleration: mode.hardware_acceleration(),
        wgpu_options,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };

    eframe::run_native(
        "Scope Analyzer",
        native_options,
        Box::new(|cc| Box::new(app::ScopeApp::new(cc))),
    )
}

fn base_scope_viewport_builder() -> eframe::egui::ViewportBuilder {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Scope Analyzer")
        .with_inner_size(INITIAL_WINDOW_SIZE)
        .with_min_inner_size(MIN_WINDOW_SIZE)
        .with_resizable(true);

    if cfg!(target_os = "windows") {
        viewport = viewport.with_maximized(true);
    }

    viewport
}

fn scope_window_icon() -> Option<eframe::egui::IconData> {
    eframe::icon_data::from_png_bytes(include_bytes!("../resources/ScopeAnalyzer.png")).ok()
}

fn configure_graphics_runtime() {
    let mut angle_dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            angle_dirs.push(parent.to_owned());
        }
    }
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let system_root = std::path::PathBuf::from(system_root);
        angle_dirs.push(system_root.join("System32").join("Microsoft-Edge-WebView"));
    }
    angle_dirs.push(std::path::PathBuf::from(
        r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application",
    ));
    angle_dirs.push(std::path::PathBuf::from(
        r"C:\Program Files (x86)\Microsoft\Edge\Application",
    ));
    angle_dirs.push(std::path::PathBuf::from(
        r"C:\Program Files\Microsoft\Edge\Application",
    ));

    let mut dll_dirs = Vec::new();
    for base in angle_dirs {
        if base.join("libEGL.dll").is_file() && base.join("libGLESv2.dll").is_file() {
            dll_dirs.push(base);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && path.join("libEGL.dll").is_file()
                && path.join("libGLESv2.dll").is_file()
            {
                dll_dirs.push(path);
            }
        }
    }

    if dll_dirs.is_empty() {
        return;
    }

    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = dll_dirs;
    paths.extend(std::env::split_paths(&old_path));
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

fn write_startup_log(message: &str) {
    let mut paths = startup_log_paths();
    for path in paths.drain(..) {
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
            continue;
        };
        let _ = writeln!(file, "{:?} {message}", SystemTime::now());
        break;
    }
}

fn startup_log_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            paths.push(parent.join("ScopeAnalyzer-startup.log"));
        }
    }
    paths.push(std::env::temp_dir().join("ScopeAnalyzer-startup.log"));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_window_starts_resizable_with_taskbar_friendly_bounds() {
        let viewport = base_scope_viewport_builder();
        assert_eq!(viewport.inner_size, Some(eframe::egui::vec2(1280.0, 760.0)));
        assert_eq!(
            viewport.min_inner_size,
            Some(eframe::egui::vec2(860.0, 520.0))
        );
        assert_eq!(viewport.resizable, Some(true));
        assert_eq!(viewport.maximized, Some(cfg!(target_os = "windows")));
    }

    #[test]
    fn default_renderer_order_prefers_cloud_desktop_safe_modes() {
        assert_eq!(DEFAULT_RENDERER_ORDER[0], RendererMode::GlowSoftware);
        assert!(
            DEFAULT_RENDERER_ORDER
                .iter()
                .position(|mode| *mode == RendererMode::GlowSoftware)
                < DEFAULT_RENDERER_ORDER
                    .iter()
                    .position(|mode| *mode == RendererMode::WgpuDx12Software)
        );
    }

    #[test]
    fn renderer_env_accepts_cloud_desktop_aliases() {
        assert_eq!(
            RendererMode::from_env_value("cloud"),
            Some(RendererMode::GlowSoftware)
        );
        assert_eq!(
            RendererMode::from_env_value("virtual"),
            Some(RendererMode::GlowSoftware)
        );
    }

    #[test]
    fn exhausted_renderer_fallback_does_not_run_wgpu_in_process() {
        let message = exhausted_renderer_message();
        assert!(message.contains("not running WGPU/WARP in-process"));
    }
}
