use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    app::ScopeApp,
    data::{CloudCsvDataSource, CsvDataSource, DatDataSource, DataSource},
    fft,
    png_export::{Canvas, Rgba},
};

const PERF_ROWS: usize = 200_000;
const PERF_CHANNELS: usize = 16;
const PERF_SAMPLE_RATE_HZ: f64 = 10_000.0;
const CLOUD_RECORD: &str = "01148c450610020000a9203d109b1f590b10f09d033ffe55011c00bb0b9df04202aa0b6cf01b03c4fa6d0d37f7b8147002590c9eff590c79ffdcfffbff26fe30fe0000000073fc4d804506100220007b2037109b1fb50edff2e8fdebf64b060601c70dc9f30bfdc00edff219fe26ef39128ffc81147102590caaff560c9dffc5ffefffc5f6c9f70000000073fc4d8057c5";

struct PerfTimer {
    label: &'static str,
    start: Instant,
}

impl PerfTimer {
    fn start(label: &'static str) -> Self {
        Self {
            label,
            start: Instant::now(),
        }
    }

    fn finish(self) -> Duration {
        let elapsed = self.start.elapsed();
        eprintln!(
            "perf:{:<28} {:>8.2} ms",
            self.label,
            elapsed.as_secs_f64() * 1000.0
        );
        if let Some(limit) = threshold(self.label) {
            assert!(
                elapsed <= limit,
                "{} took {:?}, over {:?}",
                self.label,
                elapsed,
                limit
            );
        }
        elapsed
    }
}

fn threshold(label: &str) -> Option<Duration> {
    let key = format!(
        "SCOPE_PERF_MAX_{}_MS",
        label
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect::<String>()
            .to_ascii_uppercase()
    );
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
}

fn temp_path(name: &str, extension: &str) -> PathBuf {
    let unique = format!(
        "scope_perf_{}_{}_{}.{}",
        name,
        std::process::id(),
        Instant::now().elapsed().as_nanos(),
        extension
    );
    std::env::temp_dir().join(unique)
}

fn remove_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn write_standard_csv(path: &Path, rows: usize, channels: usize) {
    let mut file = File::create(path).unwrap();
    write!(file, "time").unwrap();
    for channel in 0..channels {
        write!(file, ",CH{}", channel + 1).unwrap();
    }
    writeln!(file).unwrap();
    for row in 0..rows {
        write!(file, "{:.6}", row as f64 / PERF_SAMPLE_RATE_HZ).unwrap();
        for channel in 0..channels {
            let value = ((row as f64 * 0.01) + channel as f64).sin() * 100.0;
            write!(file, ",{value:.6}").unwrap();
        }
        writeln!(file).unwrap();
    }
}

fn write_dat(path: &Path, rows: usize, channels: usize) {
    let header_len = 1024_u32;
    let mut header = Vec::new();
    header.extend_from_slice(&header_len.to_le_bytes());
    header.extend_from_slice(&0_u32.to_le_bytes());
    header.extend_from_slice(&(PERF_SAMPLE_RATE_HZ as u32).to_le_bytes());
    header.extend_from_slice(&(channels as u32).to_le_bytes());
    for _ in 0..channels * 5 {
        header.extend_from_slice(&0_u32.to_le_bytes());
    }
    for channel in 0..channels {
        header.extend_from_slice(format!("CH{}", channel + 1).as_bytes());
        header.push(0xff);
    }
    header.resize(header_len as usize, 0xff);

    let mut file = File::create(path).unwrap();
    file.write_all(&header).unwrap();
    for row in 0..rows {
        for channel in 0..channels {
            let value = ((row as i32 + channel as i32 * 7) % 20_000 - 10_000) as i16;
            file.write_all(&value.to_le_bytes()).unwrap();
        }
    }
}

fn write_cloud_csv(path: &Path, records: usize) {
    let mut file = File::create(path).unwrap();
    writeln!(file, "Content,Ignored").unwrap();
    for _ in 0..records {
        writeln!(file, "{CLOUD_RECORD},metadata").unwrap();
    }
}

fn source_channels(source: &dyn DataSource, count: usize) -> Vec<usize> {
    (0..source.metadata().channels.len().min(count)).collect()
}

#[test]
#[ignore = "performance baseline: run explicitly with `cargo test --test-threads=1 -- --ignored perf_`"]
fn perf_csv_large_load_plot_fft() {
    let path = temp_path("csv", "csv");
    write_standard_csv(&path, PERF_ROWS, PERF_CHANNELS);

    let timer = PerfTimer::start("csv_open");
    let source = CsvDataSource::open(&path).unwrap();
    timer.finish();

    let channels = source_channels(&source, 8);
    let timer = PerfTimer::start("csv_read_zoom");
    let zoom = source.read_range(2.0, 4.0, &channels, 60_000).unwrap();
    timer.finish();
    assert!(!zoom.times.is_empty());

    let timer = PerfTimer::start("csv_summary_full");
    let summary = source
        .summarize_range(0.0, source.metadata().end_time, &channels, 1024)
        .unwrap();
    timer.finish();
    assert!(!summary.bin_start.is_empty());

    let timer = PerfTimer::start("plot_load_csv_full");
    let plot_data_loaded = ScopeApp::perf_load_plot_data(
        Arc::new(source),
        0.0,
        PERF_ROWS as f64 / PERF_SAMPLE_RATE_HZ,
        &channels,
        channels.len(),
    )
    .unwrap();
    timer.finish();
    assert!(plot_data_loaded);

    let samples = zoom.channels.first().cloned().unwrap_or_default();
    let timer = PerfTimer::start("fft_csv_zoom");
    let result = fft::analyze("CH1".to_owned(), &samples, PERF_SAMPLE_RATE_HZ, 50.0, 10);
    timer.finish();
    assert!(result.is_some());

    remove_file(&path);
}

#[test]
#[ignore = "performance baseline: run explicitly with `cargo test --test-threads=1 -- --ignored perf_`"]
fn perf_dat_large_load_plot_fft() {
    let path = temp_path("dat", "dat");
    write_dat(&path, PERF_ROWS, PERF_CHANNELS);

    let timer = PerfTimer::start("dat_open");
    let source = DatDataSource::open(&path).unwrap();
    timer.finish();

    let channels = source_channels(&source, 8);
    let timer = PerfTimer::start("dat_read_zoom");
    let zoom = source.read_range(2.0, 4.0, &channels, 60_000).unwrap();
    timer.finish();
    assert!(!zoom.times.is_empty());

    let timer = PerfTimer::start("dat_summary_full");
    let summary = source
        .summarize_range(0.0, source.metadata().end_time, &channels, 1024)
        .unwrap();
    timer.finish();
    assert!(!summary.bin_start.is_empty());

    let timer = PerfTimer::start("plot_load_dat_full");
    let plot_data_loaded = ScopeApp::perf_load_plot_data(
        Arc::new(source),
        0.0,
        PERF_ROWS as f64 / PERF_SAMPLE_RATE_HZ,
        &channels,
        channels.len(),
    )
    .unwrap();
    timer.finish();
    assert!(plot_data_loaded);

    let samples = zoom.channels.first().cloned().unwrap_or_default();
    let timer = PerfTimer::start("fft_dat_zoom");
    let result = fft::analyze("CH1".to_owned(), &samples, PERF_SAMPLE_RATE_HZ, 50.0, 10);
    timer.finish();
    assert!(result.is_some());

    remove_file(&path);
}

#[test]
#[ignore = "performance baseline: run explicitly with `cargo test --test-threads=1 -- --ignored perf_`"]
fn perf_cloud_content_large_load_plot() {
    let path = temp_path("cloud", "csv");
    write_cloud_csv(&path, PERF_ROWS / 2);

    let timer = PerfTimer::start("cloud_open");
    let source = CloudCsvDataSource::open_with_sample_rate(&path, PERF_SAMPLE_RATE_HZ).unwrap();
    timer.finish();

    let channels = source_channels(&source, 8);
    let timer = PerfTimer::start("cloud_read_zoom");
    let zoom = source.read_range(2.0, 4.0, &channels, 60_000).unwrap();
    timer.finish();
    assert!(!zoom.times.is_empty());

    let timer = PerfTimer::start("cloud_summary_full");
    let summary = source
        .summarize_range(0.0, source.metadata().end_time, &channels, 1024)
        .unwrap();
    timer.finish();
    assert!(!summary.bin_start.is_empty());

    let timer = PerfTimer::start("plot_load_cloud_full");
    let plot_data_loaded = ScopeApp::perf_load_plot_data(
        Arc::new(source),
        0.0,
        PERF_ROWS as f64 / PERF_SAMPLE_RATE_HZ,
        &channels,
        channels.len(),
    )
    .unwrap();
    timer.finish();
    assert!(plot_data_loaded);

    remove_file(&path);
}

#[test]
#[ignore = "performance baseline: run explicitly with `cargo test --test-threads=1 -- --ignored perf_`"]
fn perf_png_canvas_export_smoke() {
    let path = temp_path("png_export", "png");
    let mut canvas = Canvas::new(2400, 1200, Rgba::rgb(255, 255, 255));
    let timer = PerfTimer::start("png_canvas_draw");
    for segment in 0..4000 {
        let x0 = 20 + (segment % 2360) as i32;
        let x1 = 20 + ((segment + 1) % 2360) as i32;
        let y0 = 600 + ((segment as f64 * 0.02).sin() * 400.0) as i32;
        let y1 = 600 + (((segment + 1) as f64 * 0.02).sin() * 400.0) as i32;
        canvas.line(x0, y0, x1, y1, Rgba::rgb(25, 120, 220), 2);
    }
    timer.finish();

    let timer = PerfTimer::start("png_write");
    canvas.save_png(&path).unwrap();
    timer.finish();
    assert!(path.is_file());

    remove_file(&path);
}
