use std::{
    panic::{self, AssertUnwindSafe},
    thread::{self, JoinHandle},
};

use crate::data::DataCancelToken;

use super::ScopeApp;

#[derive(Clone, Debug)]
pub(super) struct JobCancelToken {
    data: DataCancelToken,
}

impl JobCancelToken {
    pub(super) fn new() -> Self {
        Self {
            data: DataCancelToken::new(),
        }
    }

    pub(super) fn cancel(&self) {
        self.data.cancel();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.data.is_cancelled()
    }

    pub(super) fn data_token(&self) -> DataCancelToken {
        self.data.clone()
    }
}

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

    pub(super) fn cancel_plot_jobs(&mut self) {
        if let Some(token) = &self.plot_cancel {
            token.cancel();
        }
        if let Some(token) = &self.compare_plot_cancel {
            token.cancel();
        }
    }

    pub(super) fn cancel_analysis_jobs(&mut self) {
        if let Some(token) = &self.fft_cancel {
            token.cancel();
        }
        if let Some(token) = &self.measurement_cancel {
            token.cancel();
        }
        if let Some(token) = &self.sequence_cancel {
            token.cancel();
        }
        if let Some(token) = &self.derived_curve_cancel {
            token.cancel();
        }
        if let Some(token) = &self.derived_measurement_cancel {
            token.cancel();
        }
        if let Some(token) = &self.compare_metrics_cancel {
            token.cancel();
        }
    }

    pub(super) fn cancel_import_job(&mut self) {
        if let Some(token) = &self.import_cancel {
            token.cancel();
        }
    }

    pub(super) fn any_background_job_running(&self) -> bool {
        self.plot_worker.is_some()
            || self.compare_plot_worker.is_some()
            || self.fft_worker.is_some()
            || self.measurement_worker.is_some()
            || self.sequence_worker.is_some()
            || self.derived_curve_worker.is_some()
            || self.derived_measurement_worker.is_some()
            || self.compare_metrics_worker.is_some()
            || self.import_worker.is_some()
            || self.project_save_worker.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::JobCancelToken;

    #[test]
    fn cloned_cancel_token_observes_cancellation() {
        let token = JobCancelToken::new();
        let cloned = token.clone();

        assert!(!token.is_cancelled());

        cloned.cancel();

        assert!(token.is_cancelled());
    }
}
