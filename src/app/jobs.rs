use std::{
    panic::{self, AssertUnwindSafe},
    thread::{self, JoinHandle},
};

use super::ScopeApp;

impl ScopeApp {
    pub(super) fn worker_result<T, F>(worker_message: &'static str, work: F) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, String>,
    {
        match panic::catch_unwind(AssertUnwindSafe(work)) {
            Ok(result) => result,
            Err(payload) => Err(Self::recover_worker_panic(worker_message, payload)),
        }
    }

    pub(super) fn spawn_job<T, F>(slot: &mut Option<JoinHandle<T>>, work: F)
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        *slot = Some(thread::spawn(work));
    }

    pub(super) fn take_finished_job<T>(
        slot: &mut Option<JoinHandle<T>>,
        worker_message: &'static str,
    ) -> Option<Result<T, String>> {
        let worker = slot.take_if(|worker| worker.is_finished())?;
        Some(
            worker
                .join()
                .map_err(|payload| Self::recover_worker_panic(worker_message, payload)),
        )
    }

    pub(super) fn result_matches_generation(&self, generation: u64, reload_pending: bool) -> bool {
        generation == self.data_generation && !reload_pending
    }

    pub(super) fn any_background_job_running(&self) -> bool {
        self.plot_worker.is_some()
            || self.compare_plot_worker.is_some()
            || self.fft_worker.is_some()
            || self.measurement_worker.is_some()
            || self.sequence_worker.is_some()
            || self.derived_curve_worker.is_some()
            || self.derived_measurement_worker.is_some()
            || self.import_worker.is_some()
    }
}
