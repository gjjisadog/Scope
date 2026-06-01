use std::sync::Arc;

use egui_plot::PlotPoint;

use crate::data::{DataSource, RangeSummary, SampleBlock};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PlotCacheKey {
    pub(super) generation: u64,
    pub(super) start_bits: u64,
    pub(super) end_bits: u64,
    pub(super) channels: Vec<usize>,
    pub(super) scale_bits: Vec<u32>,
    pub(super) time_offset_bits: u64,
    pub(super) plot_pixel_width: u32,
    pub(super) budget_series_count: usize,
}

pub(super) enum PlotJobData {
    Samples(SampleBlock),
    Summary(RangeSummary),
}

#[derive(Clone, Default)]
pub(super) struct PreparedPlotSeries {
    pub(super) points: Vec<Arc<[PlotPoint]>>,
    pub(super) lightweight_points: Vec<Arc<[PlotPoint]>>,
    pub(super) bounds: Vec<Option<(f64, f64)>>,
}

impl PreparedPlotSeries {
    pub(super) fn shared_points(points: &Arc<[PlotPoint]>) -> Arc<[PlotPoint]> {
        Arc::clone(points)
    }
}

#[derive(Clone, Default)]
pub(super) struct PlotSelections {
    pub(super) primary: Vec<usize>,
    pub(super) imported: Vec<Vec<usize>>,
    pub(super) derived: Vec<usize>,
}

#[derive(Clone, Default)]
pub(super) struct PanePlotSelections {
    pub(super) primary: Vec<(usize, usize)>,
    pub(super) imported: Vec<Vec<(usize, usize)>>,
    pub(super) derived: Vec<(usize, usize)>,
}

pub(super) struct PlotJobResult {
    pub(super) generation: u64,
    pub(super) key: PlotCacheKey,
    pub(super) result: Result<Option<PlotJobData>, String>,
}

pub(super) struct CompareDatasetJobResult {
    pub(super) index: usize,
    pub(super) key: PlotCacheKey,
    pub(super) result: Result<Option<PlotJobData>, String>,
}

pub(super) struct ComparePlotJobResult {
    pub(super) generation: u64,
    pub(super) datasets: Vec<CompareDatasetJobResult>,
}

pub(super) struct ComparePlotJobInput {
    pub(super) index: usize,
    pub(super) source: Arc<dyn DataSource>,
    pub(super) channels: Vec<usize>,
    pub(super) offset: f64,
    pub(super) key: PlotCacheKey,
}
