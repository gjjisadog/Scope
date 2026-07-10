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
    pub(super) envelope_low_points: Vec<Arc<[PlotPoint]>>,
    pub(super) envelope_low_lightweight_points: Vec<Arc<[PlotPoint]>>,
    pub(super) bounds: Vec<Option<(f64, f64)>>,
}

impl PreparedPlotSeries {
    pub(super) fn shared_points(points: &Arc<[PlotPoint]>) -> Arc<[PlotPoint]> {
        Arc::clone(points)
    }

    pub(super) fn subset_by_out_indices(&self, out_indices: &[usize]) -> Self {
        fn clone_selected(
            source: &[Arc<[PlotPoint]>],
            out_indices: &[usize],
        ) -> Vec<Arc<[PlotPoint]>> {
            out_indices
                .iter()
                .filter_map(|index| source.get(*index).map(Arc::clone))
                .collect()
        }

        Self {
            points: clone_selected(&self.points, out_indices),
            lightweight_points: clone_selected(&self.lightweight_points, out_indices),
            envelope_low_points: clone_selected(&self.envelope_low_points, out_indices),
            envelope_low_lightweight_points: clone_selected(
                &self.envelope_low_lightweight_points,
                out_indices,
            ),
            bounds: out_indices
                .iter()
                .filter_map(|index| self.bounds.get(*index).copied())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(y: f64) -> Arc<[PlotPoint]> {
        Arc::from([PlotPoint::new(0.0, y), PlotPoint::new(1.0, y + 1.0)])
    }

    #[test]
    fn prepared_plot_series_subset_reuses_selected_series_only() {
        let p0 = points(0.0);
        let p1 = points(10.0);
        let p2 = points(20.0);
        let low0 = points(-1.0);
        let low1 = points(-11.0);
        let low2 = points(-21.0);
        let series = PreparedPlotSeries {
            points: vec![Arc::clone(&p0), Arc::clone(&p1), Arc::clone(&p2)],
            lightweight_points: vec![Arc::clone(&p0), Arc::clone(&p1), Arc::clone(&p2)],
            envelope_low_points: vec![Arc::clone(&low0), Arc::clone(&low1), Arc::clone(&low2)],
            envelope_low_lightweight_points: vec![
                Arc::clone(&low0),
                Arc::clone(&low1),
                Arc::clone(&low2),
            ],
            bounds: vec![Some((0.0, 1.0)), Some((10.0, 11.0)), Some((20.0, 21.0))],
        };

        let subset = series.subset_by_out_indices(&[2, 0]);

        assert_eq!(subset.points.len(), 2);
        assert!(Arc::ptr_eq(&subset.points[0], &p2));
        assert!(Arc::ptr_eq(&subset.points[1], &p0));
        assert!(Arc::ptr_eq(&subset.envelope_low_points[0], &low2));
        assert!(Arc::ptr_eq(&subset.envelope_low_points[1], &low0));
        assert_eq!(subset.bounds, vec![Some((20.0, 21.0)), Some((0.0, 1.0))]);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PlotSelections {
    pub(super) primary: Vec<usize>,
    pub(super) imported: Vec<Vec<usize>>,
    pub(super) derived: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
