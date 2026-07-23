use super::*;

impl ScopeApp {
    pub(super) fn project_menu(&mut self, ui: &mut egui::Ui) {
        let project_title = if self.project_dirty {
            "Project *"
        } else {
            "Project"
        };
        ui.menu_button(project_title, |ui| {
            if self.project_save_worker.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(self.tr("正在保存工程…", "Saving project…"));
                });
                if ui.button(self.tr("取消保存", "Cancel save")).clicked() {
                    if let Some(cancel) = &self.project_save_cancel {
                        cancel.cancel();
                    }
                    ui.close_menu();
                }
                ui.separator();
            }
            if ui.button(self.tr("打开工程…", "Open Project…")).clicked() {
                self.open_project_dialog();
                ui.close_menu();
            }
            let can_save = self.project_save_worker.is_none()
                && (self.source.is_some() || !self.live.capture_history.entries().is_empty());
            if ui
                .add_enabled(
                    can_save,
                    egui::Button::new(self.tr("保存工程", "Save Project")),
                )
                .clicked()
            {
                self.save_project_dialog(false);
                ui.close_menu();
            }
            if ui
                .add_enabled(
                    can_save,
                    egui::Button::new(self.tr("工程另存为…", "Save Project As…")),
                )
                .clicked()
            {
                self.save_project_dialog(true);
                ui.close_menu();
            }
            if let Some(path) = &self.project_path {
                ui.separator();
                ui.label(RichText::new(path.display().to_string()).small());
            }
        });
    }

    pub(super) fn save_project_dialog(&mut self, save_as: bool) {
        let path = if !save_as {
            self.project_path.clone()
        } else {
            None
        }
        .or_else(|| {
            rfd::FileDialog::new()
                .add_filter("Scope Analyzer Project", &["scopeproj"])
                .set_file_name("workspace.scopeproj")
                .save_file()
        });
        let Some(mut path) = path else {
            return;
        };
        if path.extension().and_then(|value| value.to_str()) != Some("scopeproj") {
            path.set_extension("scopeproj");
        }
        if let Err(error) = self.start_project_save(path, false) {
            self.last_error = Some(error);
        }
    }

    pub(super) fn open_project_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Scope Analyzer Project", &["scopeproj"])
            .pick_file()
        else {
            return;
        };
        if let Err(error) = self.open_project_from(&path) {
            self.last_error = Some(error);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn save_project_to(&mut self, path: &Path) -> Result<(), String> {
        let input = self.prepare_project_save(path.to_path_buf(), false)?;
        let cancel = JobCancelToken::new();
        Self::write_project_input(input, &cancel)
    }

    pub(super) fn start_project_save(
        &mut self,
        path: PathBuf,
        autosave: bool,
    ) -> Result<(), String> {
        if self.project_save_worker.is_some() {
            return Err("A project save is already running.".to_owned());
        }
        let input = self.prepare_project_save(path, autosave)?;
        let cancel = JobCancelToken::new();
        let worker_cancel = cancel.clone();
        self.project_save_cancel = Some(cancel);
        Self::spawn_job(&mut self.project_save_worker, move || {
            let path = input.path.clone();
            let autosave = input.autosave;
            let result = Self::worker_result("Project save worker panicked.", || {
                Self::write_project_input(input, &worker_cancel)
            });
            ProjectSaveWorkerResult {
                path,
                autosave,
                result,
            }
        });
        Ok(())
    }

    pub(super) fn prepare_project_save(
        &self,
        path: PathBuf,
        autosave: bool,
    ) -> Result<ProjectSaveInput, String> {
        let document = self.build_project_document(&path)?;
        let captures = self
            .live
            .capture_history
            .entries()
            .iter()
            .filter_map(|entry| {
                let scope_analyzer::live::capture_history::CapturePayload::InMemory(capture) =
                    &entry.payload
                else {
                    return None;
                };
                Some(ProjectCaptureSaveSpec {
                    capture: Arc::clone(capture),
                    trigger_config: entry.trigger_config.clone(),
                    id: entry.id,
                    label: entry.label.clone(),
                    note: entry.note.clone(),
                    pinned: entry.pinned,
                    selected: self.live.capture_history.selected_id() == Some(entry.id),
                })
            })
            .collect();
        Ok(ProjectSaveInput {
            asset_project_path: if autosave {
                self.project_path.clone().unwrap_or_else(|| path.clone())
            } else {
                path.clone()
            },
            path,
            document,
            captures,
            channel_table: self.live.channel_table.clone(),
            channel_presentations: self.live.channel_presentations.clone(),
            tick_hz: self
                .live
                .hello_ack
                .as_ref()
                .map(|hello| hello.tick_hz)
                .unwrap_or(1),
            sample_rate_hz: self.live.acquisition.sample_rate_hz.max(1),
            autosave,
        })
    }

    fn build_project_document(
        &self,
        project_path: &Path,
    ) -> Result<project_file::ScopeProjectDocument, String> {
        let mut document = project_file::ScopeProjectDocument::empty(
            project_file::ProjectId(self.project_id.clone()),
            env!("CARGO_PKG_VERSION"),
        );
        if self.source.is_none() {
            document.workspace.show_channel_panel = self.show_channel_panel;
            document.workspace.show_analysis_panel = self.show_analysis_panel;
            document.workspace.layout_rows = self.scope_layout_rows;
            document.workspace.layout_cols = self.scope_layout_cols;
            document.analysis.harmonic_base_hz = self.harmonic_base_hz;
            document.compare = project_file::ProjectCompare::default();
            document.live_profile = Some(project_live_profile(&self.live));
            document.export = project_export_state(self);
            document.validate().map_err(|error| error.to_string())?;
            return Ok(document);
        }
        let Some(primary_source) = &self.source else {
            unreachable!("source presence checked above");
        };
        let primary_path = self
            .loaded_path
            .as_ref()
            .ok_or_else(|| "The primary dataset has no file path.".to_owned())?;
        let primary_source_id = project_file::SourceId("source-primary".to_owned());
        document.sources.push(project_source(
            project_path,
            primary_path,
            primary_source_id.clone(),
            self.source_kind.unwrap_or(SourceKind::Local),
            true,
        )?);
        let primary_channels = primary_source
            .metadata()
            .channels
            .iter()
            .map(|channel| project_file::ProjectChannelState {
                channel: channel_ref(&primary_source_id, channel),
                display_name: self
                    .display_names
                    .get(channel.index)
                    .cloned()
                    .unwrap_or_else(|| channel.name.clone()),
                color: self
                    .channel_colors
                    .get(channel.index)
                    .copied()
                    .unwrap_or(Color32::WHITE)
                    .to_array(),
                visible: self.visible.get(channel.index).copied().unwrap_or(false),
                scale: self
                    .channel_scales
                    .get(channel.index)
                    .copied()
                    .unwrap_or(1.0),
                pane: self.channel_panes.get(channel.index).copied().unwrap_or(0),
                line_width: self.line_widths.get(channel.index).copied().unwrap_or(1.5),
                line_pattern: format!(
                    "{:?}",
                    self.line_patterns
                        .get(channel.index)
                        .copied()
                        .unwrap_or(ChannelLinePattern::Solid)
                ),
            })
            .collect();
        document.datasets.push(project_file::ProjectDataset {
            id: project_file::DatasetId("dataset-primary".to_owned()),
            source_id: primary_source_id.clone(),
            role: project_file::ProjectDatasetRole::Primary,
            display_name: self.primary_dataset_name.clone(),
            enabled: true,
            line_pattern: "Solid".to_owned(),
            time_offset: 0.0,
            channels: primary_channels,
        });
        if self.source_kind == Some(SourceKind::Scope) {
            document
                .captures
                .extend(
                    self.scope_trigger_events
                        .iter()
                        .enumerate()
                        .map(|(ordinal, trigger)| project_file::ProjectCaptureRef {
                            id: project_file::CaptureId(format!("recording-trigger-{ordinal}")),
                            origin: project_file::ProjectCaptureOrigin::RecordingTrigger,
                            source_id: primary_source_id.clone(),
                            trigger_ordinal: Some(ordinal),
                            label: format!("Trigger {}", ordinal + 1),
                            note: if trigger.auto_timeout {
                                "Auto timeout".to_owned()
                            } else {
                                String::new()
                            },
                            pinned: false,
                            selected: self.selected_scope_trigger == Some(ordinal),
                        }),
                );
        }
        for (index, imported) in self.imported_datasets.iter().enumerate() {
            let source_id = project_file::SourceId(format!("source-imported-{index}"));
            document.sources.push(project_source(
                project_path,
                &imported.path,
                source_id.clone(),
                imported.kind,
                true,
            )?);
            document.datasets.push(project_file::ProjectDataset {
                id: project_file::DatasetId(format!("dataset-imported-{index}")),
                source_id: source_id.clone(),
                role: project_file::ProjectDatasetRole::Imported,
                display_name: imported.display_name.clone(),
                enabled: true,
                line_pattern: format!("{:?}", imported.line_pattern),
                time_offset: imported.time_offset,
                channels: imported
                    .source
                    .metadata()
                    .channels
                    .iter()
                    .map(|channel| project_file::ProjectChannelState {
                        channel: channel_ref(&source_id, channel),
                        display_name: channel.name.clone(),
                        color: Color32::WHITE.to_array(),
                        visible: imported
                            .visible
                            .get(channel.index)
                            .copied()
                            .unwrap_or(false),
                        scale: 1.0,
                        pane: 0,
                        line_width: 1.5,
                        line_pattern: format!("{:?}", imported.line_pattern),
                    })
                    .collect(),
            });
        }
        document.workspace = project_file::ProjectWorkspace {
            layout_rows: self.scope_layout_rows,
            layout_cols: self.scope_layout_cols,
            viewport: project_file::ProjectViewport {
                initialized: self.plot_viewport.initialized,
                view_start: self.plot_viewport.view_start,
                view_end: self.plot_viewport.view_end,
                y_min: self.plot_viewport.y_min.unwrap_or(-1.0),
                y_max: self.plot_viewport.y_max.unwrap_or(1.0),
                pane_y_bounds: self
                    .plot_viewport
                    .pane_y_bounds
                    .iter()
                    .map(|bounds| bounds.map_or([-1.0, 1.0], |(min, max)| [min, max]))
                    .collect(),
                active_pane: self.plot_viewport.active_scope_pane,
                cursor_a: self.plot_viewport.cursor_a,
                cursor_b: self.plot_viewport.cursor_b,
                show_cursor_a: self.plot_viewport.show_cursor_a,
                show_cursor_b: self.plot_viewport.show_cursor_b,
                active_cursor: Some(
                    match self.plot_viewport.active_cursor {
                        CursorId::A => "a",
                        CursorId::B => "b",
                    }
                    .to_owned(),
                ),
            },
            show_channel_panel: self.show_channel_panel,
            show_analysis_panel: self.show_analysis_panel,
            selected_dataset_id: Some(project_file::DatasetId("dataset-primary".to_owned())),
        };
        document.analysis.harmonic_base_hz = self.harmonic_base_hz;
        document.analysis.fft_channel = primary_source
            .metadata()
            .channels
            .get(self.fft_channel)
            .map(|channel| channel_ref(&primary_source_id, channel));
        document.analysis.sequence_channels = channel_refs(
            primary_source.metadata(),
            &primary_source_id,
            &self.sequence_channels,
        );
        document.analysis.pll_channels = channel_refs(
            primary_source.metadata(),
            &primary_source_id,
            &self.pll_source_channels,
        );
        document.analysis.dq_channels = channel_refs(
            primary_source.metadata(),
            &primary_source_id,
            &self.dq_source_channels,
        );
        if self.power_enabled {
            let voltage = channel_ref_array(
                primary_source.metadata(),
                &primary_source_id,
                self.power_voltage_channels,
            )?;
            let current = channel_ref_array(
                primary_source.metadata(),
                &primary_source_id,
                self.power_current_channels,
            )?;
            document.analysis.power = Some(project_file::ProjectPowerBindings {
                voltage,
                current,
                voltage_scales: self
                    .power_voltage_channels
                    .map(|channel| f64::from(self.channel_scale(channel))),
                current_scales: self
                    .power_current_channels
                    .map(|channel| f64::from(self.channel_scale(channel))),
            });
        }
        document.analysis.derived_curves = self
            .repid_derived_curves
            .iter()
            .enumerate()
            .map(|(index, curve)| project_file::ProjectDerivedCurve {
                name: curve.name.clone(),
                script: curve.script.clone(),
                gain: curve.k,
                offset: curve.b,
                visible: self.derived_visible.get(index).copied().unwrap_or(false),
                pane: self.derived_panes.get(index).copied().unwrap_or(0),
            })
            .collect();
        document.compare = self.compare_config.clone();
        document.export = project_export_state(self);
        document.live_profile = Some(project_live_profile(&self.live));
        document.validate().map_err(|error| error.to_string())?;
        Ok(document)
    }

    pub(super) fn write_project_input(
        mut input: ProjectSaveInput,
        cancel: &JobCancelToken,
    ) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("Project save cancelled.".to_owned());
        }
        let Some(table) = &input.channel_table else {
            return project_file::save_project_atomic(&input.path, &input.document)
                .map_err(|error| error.to_string());
        };
        let project_dir = input
            .asset_project_path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let stem = input
            .asset_project_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace.scopeproj");
        let relative_assets = format!("{stem}.assets/captures");
        let asset_dir = project_dir.join(&relative_assets);
        fs::create_dir_all(&asset_dir).map_err(|error| error.to_string())?;
        for capture_spec in input.captures {
            if cancel.is_cancelled() {
                return Err("Project save cancelled.".to_owned());
            }
            let file_name = format!("capture-{}.scope", capture_spec.id.0);
            let asset_path = asset_dir.join(&file_name);
            let write_result =
                scope_analyzer::live::recording::write_capture_scope_file_with_cancel(
                    &asset_path,
                    &capture_spec.capture,
                    &capture_spec.trigger_config,
                    scope_analyzer::live::recording::CaptureScopeContext {
                        source_table: table,
                        channel_presentations: &input.channel_presentations,
                        tick_hz: input.tick_hz,
                        sample_rate_hz: input.sample_rate_hz,
                        client_version: env!("CARGO_PKG_VERSION"),
                    },
                    || cancel.is_cancelled(),
                );
            match write_result {
                Ok(()) => {}
                Err(scope_analyzer::live::recording::RecordingError::Cancelled) => {
                    return Err("Project save cancelled.".to_owned());
                }
                Err(error) => return Err(error.to_string()),
            }
            let source_id = project_file::SourceId(format!("capture-source-{}", capture_spec.id.0));
            let mut capture_source = project_source(
                &input.path,
                &asset_path,
                source_id.clone(),
                SourceKind::Scope,
                false,
            )?;
            capture_source.kind = project_file::ProjectSourceKind::CaptureAsset;
            input.document.sources.push(capture_source);
            input
                .document
                .captures
                .push(project_file::ProjectCaptureRef {
                    id: project_file::CaptureId(format!("capture-{}", capture_spec.id.0)),
                    origin: project_file::ProjectCaptureOrigin::CaptureAsset,
                    source_id,
                    trigger_ordinal: Some(0),
                    label: capture_spec.label,
                    note: capture_spec.note,
                    pinned: capture_spec.pinned,
                    selected: capture_spec.selected,
                });
        }
        if cancel.is_cancelled() {
            return Err("Project save cancelled.".to_owned());
        }
        input
            .document
            .validate()
            .map_err(|error| error.to_string())?;
        project_file::save_project_atomic(&input.path, &input.document)
            .map_err(|error| error.to_string())
    }

    pub(super) fn open_project_from(&mut self, path: &Path) -> Result<(), String> {
        let autosave = path.with_extension("scopeproj.autosave");
        let restore_path = if autosave.is_file()
            && fs::metadata(&autosave)
                .and_then(|metadata| metadata.modified())
                .ok()
                > fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
        {
            &autosave
        } else {
            path
        };
        let mut document =
            project_file::load_project(restore_path).map_err(|error| error.to_string())?;
        let mut resolutions = project_file::resolve_project_sources(path, &document);
        let mut relocated_sources = false;
        for resolution in &mut resolutions {
            if !matches!(
                resolution.resolution,
                project_file::ProjectSourceResolution::Missing
            ) {
                continue;
            }
            let source = document
                .sources
                .iter()
                .find(|source| source.id == resolution.source_id)
                .expect("resolution source belongs to document");
            if !source.required {
                continue;
            }
            let replacement = rfd::FileDialog::new()
                .set_title(format!("Relocate {}", source.file.relative_path))
                .pick_file()
                .ok_or_else(|| {
                    format!(
                        "Project restore cancelled: missing source {}",
                        source.file.relative_path
                    )
                })?;
            project_file::relocate_project_source(
                path,
                &mut document,
                &resolution.source_id,
                &replacement,
            )
            .map_err(|error| error.to_string())?;
            relocated_sources = true;
        }
        resolutions = project_file::resolve_project_sources(path, &document);
        let mut staged = Vec::new();
        for dataset in &document.datasets {
            let source_path = resolutions
                .iter()
                .find(|resolution| resolution.source_id == dataset.source_id)
                .and_then(|resolution| match &resolution.resolution {
                    project_file::ProjectSourceResolution::Resolved(path)
                    | project_file::ProjectSourceResolution::MetadataMismatch(path) => {
                        Some(path.clone())
                    }
                    project_file::ProjectSourceResolution::Missing => None,
                })
                .ok_or_else(|| format!("Dataset source {} is missing", dataset.source_id.0))?;
            let opened = Self::open_waveform_file(&source_path, self.sample_rate_hz)?;
            staged.push((dataset.clone(), opened));
        }
        if staged.is_empty() {
            self.clear_all_datasets();
        } else {
            let primary_position = staged
                .iter()
                .position(|(dataset, _)| dataset.role == project_file::ProjectDatasetRole::Primary)
                .ok_or_else(|| "Project has no primary dataset".to_owned())?;
            let (primary_dataset, primary) = staged.remove(primary_position);
            let primary_path = primary.path.clone();
            let primary_kind = primary.kind;
            self.clear_imported_datasets();
            self.set_source(primary.source, primary.path, primary.kind);
            if primary_kind == SourceKind::Scope {
                let recording =
                    scope_analyzer::live::recording::ScopeRecording::open(&primary_path)
                        .map_err(|error| error.to_string())?;
                self.scope_trigger_events = recording.triggers().to_vec();
                self.scope_trigger_tick_hz = recording.metadata().tick_hz;
                self.selected_scope_trigger = document
                    .captures
                    .iter()
                    .find(|capture| {
                        capture.origin == project_file::ProjectCaptureOrigin::RecordingTrigger
                            && capture.selected
                            && capture.source_id == primary_dataset.source_id
                    })
                    .and_then(|capture| capture.trigger_ordinal)
                    .or_else(|| (!self.scope_trigger_events.is_empty()).then_some(0));
            }
            apply_primary_dataset(self, &primary_dataset);
            for (dataset, opened) in staged {
                self.add_imported_dataset(opened.source, opened.path, opened.kind);
                if let Some(imported) = self.imported_datasets.last_mut() {
                    imported.display_name = dataset.display_name;
                    imported.time_offset = dataset.time_offset;
                    imported.line_pattern = parse_line_pattern(&dataset.line_pattern);
                    for channel in dataset.channels {
                        if let Some(visible) = imported.visible.get_mut(channel.channel.index_hint)
                        {
                            *visible = channel.visible;
                        }
                    }
                }
            }
        }
        apply_project_workspace(self, &document);
        apply_project_analysis(self, &document);
        apply_project_compare(self, &document);
        apply_project_export(self, &document.export);
        apply_live_profile(&mut self.live, document.live_profile.as_ref())?;
        restore_project_captures(&mut self.live, &document, &resolutions)?;
        self.finish_project_restore(path, document.project_id.0, relocated_sources);
        self.import_status = Some(if restore_path == autosave {
            format!("Recovered newer project autosave: {}", autosave.display())
        } else {
            format!("Project restored: {}", path.display())
        });
        Ok(())
    }

    pub(super) fn finish_project_restore(
        &mut self,
        path: &Path,
        project_id: String,
        relocated_sources: bool,
    ) {
        self.project_path = Some(path.to_path_buf());
        self.project_id = project_id;
        self.project_dirty = relocated_sources;
        self.project_last_autosave = Instant::now();
    }

    pub(super) fn autosave_project_if_due(&mut self) {
        if self.project_save_worker.is_some()
            || !self.project_dirty
            || self.project_last_autosave.elapsed() < Duration::from_secs(30)
        {
            return;
        }
        let Some(path) = self.project_path.clone() else {
            return;
        };
        let autosave = path.with_extension("scopeproj.autosave");
        if let Err(error) = self.start_project_save(autosave, true) {
            self.last_error = Some(format!("Project autosave failed: {error}"));
        }
        self.project_last_autosave = Instant::now();
    }

    pub(super) fn poll_project_save_worker(&mut self) {
        let Some(joined) = Self::take_finished_job(
            &mut self.project_save_worker,
            "Project save worker panicked.",
        ) else {
            return;
        };
        self.project_save_cancel = None;
        match joined {
            Err(error) => self.last_error = Some(error),
            Ok(ProjectSaveWorkerResult {
                path: _,
                autosave,
                result: Err(error),
            }) => {
                if error == "Project save cancelled." {
                    self.import_status = Some(error);
                } else {
                    self.last_error = Some(if autosave {
                        format!("Project autosave failed: {error}")
                    } else {
                        error
                    });
                }
            }
            Ok(ProjectSaveWorkerResult {
                path,
                autosave: true,
                result: Ok(()),
            }) => {
                self.import_status = Some(format!("Project recovery saved: {}", path.display()));
            }
            Ok(ProjectSaveWorkerResult {
                path,
                autosave: false,
                result: Ok(()),
            }) => {
                self.project_path = Some(path.clone());
                self.project_dirty = false;
                self.project_last_autosave = Instant::now();
                self.import_status = Some(format!("Project saved: {}", path.display()));
            }
        }
    }
}

fn project_source(
    project_path: &Path,
    path: &Path,
    id: project_file::SourceId,
    kind: SourceKind,
    required: bool,
) -> Result<project_file::ProjectSource, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let project_dir = project_path.parent().unwrap_or_else(|| Path::new("."));
    let relative_path = path
        .strip_prefix(project_dir)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_owned)
        .or_else(|| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| format!("Unsafe source path: {}", path.display()))?;
    Ok(project_file::ProjectSource {
        id,
        kind: match kind {
            SourceKind::Dat => project_file::ProjectSourceKind::Dat,
            SourceKind::Scope => project_file::ProjectSourceKind::Scope,
            SourceKind::Cloud | SourceKind::Local => project_file::ProjectSourceKind::Csv,
        },
        file: project_file::ProjectFileRef {
            relative_path,
            absolute_hint: path.to_string_lossy().into_owned(),
            size_bytes: metadata.len(),
            modified_unix_ms: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
            partial_crc32c: None,
        },
        required,
    })
}

fn channel_ref(
    source_id: &project_file::SourceId,
    channel: &crate::data::ChannelMeta,
) -> project_file::ProjectChannelRef {
    project_file::ProjectChannelRef {
        source_id: source_id.clone(),
        raw_name: channel.name.clone(),
        index_hint: channel.index,
        channel_id_hint: None,
    }
}

fn channel_refs(
    metadata: &DatasetMeta,
    source_id: &project_file::SourceId,
    channels: &[usize],
) -> Vec<project_file::ProjectChannelRef> {
    channels
        .iter()
        .filter_map(|index| metadata.channels.get(*index))
        .map(|channel| channel_ref(source_id, channel))
        .collect()
}

fn channel_ref_array(
    metadata: &DatasetMeta,
    source_id: &project_file::SourceId,
    channels: [usize; 3],
) -> Result<[project_file::ProjectChannelRef; 3], String> {
    let refs = channel_refs(metadata, source_id, &channels);
    refs.try_into()
        .map_err(|_| "Three-phase binding references missing channels".to_owned())
}

fn project_live_profile(
    live: &scope_analyzer::live::state::LiveScopeState,
) -> project_file::ProjectLiveProfile {
    let trigger = live.trigger.config();
    project_file::ProjectLiveProfile {
        transport: Some(match &live.transport {
            scope_analyzer::live::transport::TransportConfig::Tcp { address } => {
                project_file::ProjectTransport::Tcp {
                    address: address.clone(),
                }
            }
            scope_analyzer::live::transport::TransportConfig::Serial { port, baud } => {
                project_file::ProjectTransport::Serial {
                    port: port.clone(),
                    baud: *baud,
                }
            }
        }),
        sample_rate_hz: live.acquisition.sample_rate_hz,
        batch_samples: live.acquisition.batch_samples,
        channel_ids: live
            .channel_table
            .as_ref()
            .map(|table| {
                table
                    .channels
                    .iter()
                    .filter(|channel| {
                        live.acquisition.channel_mask & (1_u64 << channel.channel_id) != 0
                    })
                    .map(|channel| channel.channel_id)
                    .collect()
            })
            .unwrap_or_default(),
        trigger: project_file::ProjectTriggerConfig {
            mode: match trigger.mode {
                scope_analyzer::live::trigger::TriggerMode::Auto => {
                    project_file::ProjectTriggerMode::Auto
                }
                scope_analyzer::live::trigger::TriggerMode::Normal => {
                    project_file::ProjectTriggerMode::Normal
                }
                scope_analyzer::live::trigger::TriggerMode::Single => {
                    project_file::ProjectTriggerMode::Single
                }
            },
            edge: match trigger.edge {
                scope_analyzer::live::trigger::TriggerEdge::Rising => {
                    project_file::ProjectTriggerEdge::Rising
                }
                scope_analyzer::live::trigger::TriggerEdge::Falling => {
                    project_file::ProjectTriggerEdge::Falling
                }
                scope_analyzer::live::trigger::TriggerEdge::Either => {
                    project_file::ProjectTriggerEdge::Either
                }
            },
            source_channel: trigger.source_channel,
            level: trigger.level,
            hysteresis: trigger.hysteresis,
            pre_samples: trigger.pre_samples,
            post_samples: trigger.post_samples,
            auto_timeout_samples: trigger.auto_timeout_samples,
        },
        capture_history_entries: 100,
        capture_history_bytes: 128 * 1024 * 1024,
    }
}

fn apply_primary_dataset(app: &mut ScopeApp, dataset: &project_file::ProjectDataset) {
    app.primary_dataset_name = dataset.display_name.clone();
    for channel in &dataset.channels {
        let index = channel.channel.index_hint;
        if let Some(value) = app.visible.get_mut(index) {
            *value = channel.visible;
        }
        if let Some(value) = app.display_names.get_mut(index) {
            *value = channel.display_name.clone();
        }
        if let Some(value) = app.channel_colors.get_mut(index) {
            *value = Color32::from_rgba_unmultiplied(
                channel.color[0],
                channel.color[1],
                channel.color[2],
                channel.color[3],
            );
        }
        if let Some(value) = app.channel_scales.get_mut(index) {
            *value = channel.scale;
        }
        if let Some(value) = app.channel_panes.get_mut(index) {
            *value = channel.pane;
        }
        if let Some(value) = app.line_widths.get_mut(index) {
            *value = channel.line_width;
        }
        if let Some(value) = app.line_patterns.get_mut(index) {
            *value = parse_line_pattern(&channel.line_pattern);
        }
    }
}

fn apply_project_workspace(app: &mut ScopeApp, document: &project_file::ScopeProjectDocument) {
    let workspace = &document.workspace;
    app.scope_layout_rows = workspace.layout_rows;
    app.scope_layout_cols = workspace.layout_cols;
    app.show_channel_panel = workspace.show_channel_panel;
    app.show_analysis_panel = workspace.show_analysis_panel;
    app.plot_viewport = PlotViewport {
        initialized: workspace.viewport.initialized,
        view_start: workspace.viewport.view_start,
        view_end: workspace.viewport.view_end,
        y_min: Some(workspace.viewport.y_min),
        y_max: Some(workspace.viewport.y_max),
        pane_y_bounds: workspace
            .viewport
            .pane_y_bounds
            .iter()
            .map(|bounds| Some((bounds[0], bounds[1])))
            .collect(),
        active_scope_pane: workspace.viewport.active_pane,
        cursor_a: workspace.viewport.cursor_a,
        cursor_b: workspace.viewport.cursor_b,
        show_cursor_a: workspace.viewport.show_cursor_a,
        show_cursor_b: workspace.viewport.show_cursor_b,
        active_cursor: if workspace.viewport.active_cursor.as_deref() == Some("b") {
            CursorId::B
        } else {
            CursorId::A
        },
    };
    app.plot_viewport.set_pane_count(app.scope_pane_count());
    app.needs_plot_reload = true;
    app.needs_compare_plot_reload = true;
}

fn apply_project_analysis(app: &mut ScopeApp, document: &project_file::ScopeProjectDocument) {
    let analysis = &document.analysis;
    app.harmonic_base_hz = analysis.harmonic_base_hz;
    if let Some(channel) = &analysis.fft_channel {
        app.fft_channel = channel.index_hint;
    }
    if analysis.sequence_channels.len() == 3 {
        app.sequence_channels = [
            analysis.sequence_channels[0].index_hint,
            analysis.sequence_channels[1].index_hint,
            analysis.sequence_channels[2].index_hint,
        ];
    }
    if analysis.pll_channels.len() == 3 {
        app.pll_source_channels = [
            analysis.pll_channels[0].index_hint,
            analysis.pll_channels[1].index_hint,
            analysis.pll_channels[2].index_hint,
        ];
    }
    if analysis.dq_channels.len() == 3 {
        app.dq_source_channels = [
            analysis.dq_channels[0].index_hint,
            analysis.dq_channels[1].index_hint,
            analysis.dq_channels[2].index_hint,
        ];
    }
    app.power_enabled = analysis.power.is_some();
    if let Some(power) = &analysis.power {
        app.power_voltage_channels = power.voltage.each_ref().map(|channel| channel.index_hint);
        app.power_current_channels = power.current.each_ref().map(|channel| channel.index_hint);
    }
    app.repid_derived_curves = analysis
        .derived_curves
        .iter()
        .map(|curve| crate::repid_derived::RepidDerivedCurve {
            name: curve.name.clone(),
            raw_name: curve.name.clone(),
            script_vars: Vec::new(),
            script: curve.script.clone(),
            min: None,
            max: None,
            k: curve.gain,
            b: curve.offset,
            color: None,
            pen_style: None,
            auto_color: true,
            time_offset: 0.0,
            time_scale: 1.0,
        })
        .collect();
    let derived_count = app.derived_output_count();
    app.derived_visible.resize(derived_count, false);
    app.derived_panes.resize(derived_count, 0);
    for (index, curve) in analysis.derived_curves.iter().enumerate() {
        if let Some(value) = app.derived_visible.get_mut(index) {
            *value = curve.visible;
        }
        if let Some(value) = app.derived_panes.get_mut(index) {
            *value = curve.pane;
        }
    }
    app.measurement_cache = None;
    app.needs_fft_reload = true;
    app.needs_derived_reload = true;
}

fn apply_project_compare(app: &mut ScopeApp, document: &project_file::ScopeProjectDocument) {
    app.compare_config = document.compare.clone();
    app.compare_result = None;
    app.compare_status = None;
}

fn project_export_state(app: &ScopeApp) -> project_file::ProjectExportState {
    let width = app.export_preview_size[0];
    let height = app.export_preview_size[1];
    let mut annotations = Vec::new();
    if width > 0 && height > 0 {
        let point = |point: CanvasPoint| {
            [
                (point.x as f32 / width as f32).clamp(0.0, 1.0),
                (point.y as f32 / height as f32).clamp(0.0, 1.0),
            ]
        };
        annotations.extend(app.export_text_annotations.iter().map(|annotation| {
            project_file::ProjectAnnotation {
                kind: project_file::ProjectAnnotationKind::Text,
                text: annotation.text.clone(),
                points: vec![point(annotation.position)],
                color: annotation_color_array(annotation.color),
                width: annotation.scale.max(1) as f32,
            }
        }));
        annotations.extend(app.export_arrow_annotations.iter().map(|annotation| {
            project_file::ProjectAnnotation {
                kind: project_file::ProjectAnnotationKind::Arrow,
                text: String::new(),
                points: vec![point(annotation.start), point(annotation.end)],
                color: annotation_color_array(annotation.color),
                width: annotation.width.max(1) as f32,
            }
        }));
        annotations.extend(app.export_rectangle_annotations.iter().map(|annotation| {
            project_file::ProjectAnnotation {
                kind: project_file::ProjectAnnotationKind::Rectangle,
                text: if annotation.kind == ShapeKind::Ellipse {
                    "ellipse"
                } else {
                    "rectangle"
                }
                .to_owned(),
                points: vec![
                    point(CanvasPoint::new(annotation.rect.left, annotation.rect.top)),
                    point(CanvasPoint::new(
                        annotation.rect.right,
                        annotation.rect.bottom,
                    )),
                ],
                color: annotation_color_array(annotation.color),
                width: annotation.width.max(1) as f32,
            }
        }));
        annotations.extend(app.export_ink_strokes.iter().map(|annotation| {
            project_file::ProjectAnnotation {
                kind: project_file::ProjectAnnotationKind::Ink,
                text: String::new(),
                points: annotation.points.iter().copied().map(point).collect(),
                color: annotation_color_array(annotation.color),
                width: annotation.width.max(1) as f32,
            }
        }));
    }
    project_file::ProjectExportState {
        format: app.export_image_format.extension().to_owned(),
        dpi: app.export_dpi_value,
        include_cursor_table: app.export_cursor_table_enabled,
        canvas_width: width,
        canvas_height: height,
        annotations,
    }
}

fn apply_project_export(app: &mut ScopeApp, export: &project_file::ProjectExportState) {
    app.export_image_format = if export.format.eq_ignore_ascii_case("svg") {
        ExportImageFormat::Svg
    } else {
        ExportImageFormat::Png
    };
    app.export_dpi_value = export.dpi.clamp(72, 1200);
    app.export_dpi = match export.dpi {
        600.. => ExportDpi::Dpi600,
        300..=599 => ExportDpi::Dpi300,
        _ => ExportDpi::Dpi150,
    };
    app.export_cursor_table_enabled = export.include_cursor_table;
    app.export_preview_size = [export.canvas_width, export.canvas_height];
    app.export_text_annotations.clear();
    app.export_arrow_annotations.clear();
    app.export_rectangle_annotations.clear();
    app.export_ink_strokes.clear();
    let point = |value: [f32; 2]| {
        CanvasPoint::new(
            (value[0] * export.canvas_width as f32).round() as i32,
            (value[1] * export.canvas_height as f32).round() as i32,
        )
    };
    for annotation in &export.annotations {
        let color = annotation_color(annotation.color);
        match annotation.kind {
            project_file::ProjectAnnotationKind::Text if !annotation.points.is_empty() => {
                app.export_text_annotations.push(ExportTextAnnotation {
                    text: annotation.text.clone(),
                    position: point(annotation.points[0]),
                    color,
                    scale: annotation.width.round().max(1.0) as i32,
                });
            }
            project_file::ProjectAnnotationKind::Arrow if annotation.points.len() >= 2 => {
                app.export_arrow_annotations.push(ExportArrowAnnotation {
                    start: point(annotation.points[0]),
                    end: point(annotation.points[1]),
                    color,
                    width: annotation.width.round().max(1.0) as i32,
                    head_size: 12.0,
                    line_style: AnnotationLineStyle::Solid,
                });
            }
            project_file::ProjectAnnotationKind::Rectangle if annotation.points.len() >= 2 => {
                app.export_rectangle_annotations.push(
                    ExportRectangleAnnotation::from_corners_with_kind(
                        point(annotation.points[0]),
                        point(annotation.points[1]),
                        if annotation.text == "ellipse" {
                            ShapeKind::Ellipse
                        } else {
                            ShapeKind::Rectangle
                        },
                        color,
                        annotation.width.round().max(1.0) as i32,
                        AnnotationLineStyle::Solid,
                    ),
                );
            }
            project_file::ProjectAnnotationKind::Ink if !annotation.points.is_empty() => {
                app.export_ink_strokes.push(ExportInkStroke {
                    points: annotation.points.iter().copied().map(point).collect(),
                    color,
                    width: annotation.width.round().max(1.0) as i32,
                });
            }
            _ => {}
        }
    }
    app.export_preview_dirty = true;
}

fn annotation_color_array(color: AnnotationColor) -> [u8; 4] {
    [color.r, color.g, color.b, color.a]
}

fn annotation_color(color: [u8; 4]) -> AnnotationColor {
    AnnotationColor {
        r: color[0],
        g: color[1],
        b: color[2],
        a: color[3],
    }
}

fn apply_live_profile(
    live: &mut scope_analyzer::live::state::LiveScopeState,
    profile: Option<&project_file::ProjectLiveProfile>,
) -> Result<(), String> {
    let Some(profile) = profile else {
        return Ok(());
    };
    live.transport = match profile.transport.as_ref() {
        Some(project_file::ProjectTransport::Tcp { address }) => {
            scope_analyzer::live::transport::TransportConfig::Tcp {
                address: address.clone(),
            }
        }
        Some(project_file::ProjectTransport::Serial { port, baud }) => {
            scope_analyzer::live::transport::TransportConfig::Serial {
                port: port.clone(),
                baud: *baud,
            }
        }
        None => live.transport.clone(),
    };
    live.acquisition.sample_rate_hz = profile.sample_rate_hz;
    live.acquisition.batch_samples = profile.batch_samples;
    if !profile.channel_ids.is_empty() {
        live.acquisition.channel_mask = profile
            .channel_ids
            .iter()
            .fold(0_u64, |mask, channel| mask | (1_u64 << channel));
    }
    let trigger = scope_analyzer::live::trigger::TriggerConfig {
        mode: match profile.trigger.mode {
            project_file::ProjectTriggerMode::Auto => {
                scope_analyzer::live::trigger::TriggerMode::Auto
            }
            project_file::ProjectTriggerMode::Normal => {
                scope_analyzer::live::trigger::TriggerMode::Normal
            }
            project_file::ProjectTriggerMode::Single => {
                scope_analyzer::live::trigger::TriggerMode::Single
            }
        },
        edge: match profile.trigger.edge {
            project_file::ProjectTriggerEdge::Rising => {
                scope_analyzer::live::trigger::TriggerEdge::Rising
            }
            project_file::ProjectTriggerEdge::Falling => {
                scope_analyzer::live::trigger::TriggerEdge::Falling
            }
            project_file::ProjectTriggerEdge::Either => {
                scope_analyzer::live::trigger::TriggerEdge::Either
            }
        },
        source_channel: profile.trigger.source_channel,
        level: profile.trigger.level,
        hysteresis: profile.trigger.hysteresis,
        pre_samples: profile.trigger.pre_samples,
        post_samples: profile.trigger.post_samples,
        auto_timeout_samples: profile.trigger.auto_timeout_samples,
    };
    live.set_trigger_config(trigger)?;
    live.configuration_applied = false;
    Ok(())
}

fn restore_project_captures(
    live: &mut scope_analyzer::live::state::LiveScopeState,
    document: &project_file::ScopeProjectDocument,
    resolutions: &[project_file::ResolvedProjectSource],
) -> Result<(), String> {
    live.capture_history.clear(true);
    live.last_capture = None;
    for capture_ref in &document.captures {
        if capture_ref.origin != project_file::ProjectCaptureOrigin::CaptureAsset {
            continue;
        }
        let Some(path) = resolutions
            .iter()
            .find(|resolution| resolution.source_id == capture_ref.source_id)
            .and_then(|resolution| match &resolution.resolution {
                project_file::ProjectSourceResolution::Resolved(path)
                | project_file::ProjectSourceResolution::MetadataMismatch(path) => Some(path),
                project_file::ProjectSourceResolution::Missing => None,
            })
        else {
            continue;
        };
        let asset = scope_analyzer::live::recording::read_capture_scope_file(path)
            .map_err(|error| error.to_string())?;
        if live.channel_table.is_none() {
            live.channel_table = Some(asset.metadata.channel_table.clone());
            live.hello_ack = Some(scope_analyzer::live::protocol::HelloAck {
                device_capabilities: 0,
                max_payload: scope_analyzer::live::protocol::MAX_PAYLOAD_LEN as u32,
                tick_hz: asset.metadata.tick_hz,
                channel_count: u16::try_from(asset.metadata.channel_table.channels.len())
                    .unwrap_or(u16::MAX),
                max_batch_samples: asset.metadata.batch_samples,
                device_id: [0; 16],
                firmware_name: asset.metadata.firmware_name.clone(),
            });
            live.channel_presentations
                .extend(asset.metadata.channel_presentations.clone());
        }
        let capture_id = capture_history_id_from_project_ref(capture_ref)?;
        let capture = asset.capture.clone();
        let outcome = live
            .capture_history
            .insert_live_with_id(
                capture_id,
                asset.capture,
                asset.config,
                0,
                capture_ref.selected,
            )
            .map_err(|error| error.to_string())?;
        live.capture_history.set_metadata(
            outcome.id,
            capture_ref.label.clone(),
            capture_ref.note.clone(),
            capture_ref.pinned,
        );
        if capture_ref.selected {
            live.last_capture = Some(capture);
        }
    }
    if live.last_capture.is_none() {
        live.last_capture = live.selected_trigger_capture().cloned();
    }
    Ok(())
}

fn capture_history_id_from_project_ref(
    capture_ref: &project_file::ProjectCaptureRef,
) -> Result<scope_analyzer::live::capture_history::CaptureId, String> {
    let Some(value) = capture_ref.id.0.strip_prefix("capture-") else {
        return Err(format!(
            "Capture asset {} does not use a persisted numeric capture ID",
            capture_ref.id.0
        ));
    };
    let id = value.parse::<u64>().map_err(|_| {
        format!(
            "Capture asset {} does not use a persisted numeric capture ID",
            capture_ref.id.0
        )
    })?;
    if id == 0 || value != id.to_string() {
        return Err(format!(
            "Capture asset {} does not use a persisted numeric capture ID",
            capture_ref.id.0
        ));
    }
    Ok(scope_analyzer::live::capture_history::CaptureId(id))
}

fn parse_line_pattern(value: &str) -> ChannelLinePattern {
    match value {
        "Dashed" => ChannelLinePattern::Dashed,
        "DashedShort" => ChannelLinePattern::DashedShort,
        "DashedLong" => ChannelLinePattern::DashedLong,
        "Dotted" => ChannelLinePattern::Dotted,
        "DottedDense" => ChannelLinePattern::DottedDense,
        "DottedLoose" => ChannelLinePattern::DottedLoose,
        _ => ChannelLinePattern::Solid,
    }
}
