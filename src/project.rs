use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROJECT_TYPE: &str = "scope-analyzer-project";
pub const PROJECT_SCHEMA_VERSION: u32 = 2;
const LEGACY_PROJECT_SCHEMA_VERSION: u32 = 1;
pub const MAX_PROJECT_JSON_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectSourceResolution {
    Resolved(PathBuf),
    Missing,
    MetadataMismatch(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProjectSource {
    pub source_id: SourceId,
    pub resolution: ProjectSourceResolution,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DatasetId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CaptureId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectSourceKind {
    Csv,
    Dat,
    Scope,
    CaptureAsset,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFileRef {
    pub relative_path: String,
    #[serde(default)]
    pub absolute_hint: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub modified_unix_ms: Option<u64>,
    #[serde(default)]
    pub partial_crc32c: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSource {
    pub id: SourceId,
    pub kind: ProjectSourceKind,
    pub file: ProjectFileRef,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChannelRef {
    pub source_id: SourceId,
    pub raw_name: String,
    pub index_hint: usize,
    #[serde(default)]
    pub channel_id_hint: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChannelState {
    pub channel: ProjectChannelRef,
    #[serde(default)]
    pub display_name: String,
    pub color: [u8; 4],
    #[serde(default)]
    pub visible: bool,
    #[serde(default = "default_scale")]
    pub scale: f32,
    #[serde(default)]
    pub pane: usize,
    #[serde(default = "default_line_width")]
    pub line_width: f32,
    #[serde(default)]
    pub line_pattern: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectDatasetRole {
    Primary,
    Imported,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDataset {
    pub id: DatasetId,
    pub source_id: SourceId,
    pub role: ProjectDatasetRole,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub line_pattern: String,
    #[serde(default)]
    pub time_offset: f64,
    #[serde(default)]
    pub channels: Vec<ProjectChannelState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectViewport {
    pub initialized: bool,
    pub view_start: f64,
    pub view_end: f64,
    pub y_min: f64,
    pub y_max: f64,
    #[serde(default)]
    pub pane_y_bounds: Vec<[f64; 2]>,
    #[serde(default)]
    pub active_pane: usize,
    pub cursor_a: f64,
    pub cursor_b: f64,
    #[serde(default)]
    pub show_cursor_a: bool,
    #[serde(default)]
    pub show_cursor_b: bool,
    #[serde(default)]
    pub active_cursor: Option<String>,
}

impl Default for ProjectViewport {
    fn default() -> Self {
        Self {
            initialized: false,
            view_start: 0.0,
            view_end: 1.0,
            y_min: -1.0,
            y_max: 1.0,
            pane_y_bounds: Vec::new(),
            active_pane: 0,
            cursor_a: 0.25,
            cursor_b: 0.75,
            show_cursor_a: true,
            show_cursor_b: true,
            active_cursor: Some("a".to_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkspace {
    #[serde(default = "default_layout_axis")]
    pub layout_rows: usize,
    #[serde(default = "default_layout_axis")]
    pub layout_cols: usize,
    #[serde(default)]
    pub viewport: ProjectViewport,
    #[serde(default = "default_true")]
    pub show_channel_panel: bool,
    #[serde(default = "default_true")]
    pub show_analysis_panel: bool,
    #[serde(default)]
    pub selected_dataset_id: Option<DatasetId>,
}

impl Default for ProjectWorkspace {
    fn default() -> Self {
        Self {
            layout_rows: default_layout_axis(),
            layout_cols: default_layout_axis(),
            viewport: ProjectViewport::default(),
            show_channel_panel: true,
            show_analysis_panel: true,
            selected_dataset_id: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPowerBindings {
    pub voltage: [ProjectChannelRef; 3],
    pub current: [ProjectChannelRef; 3],
    #[serde(default = "default_scales")]
    pub voltage_scales: [f64; 3],
    #[serde(default = "default_scales")]
    pub current_scales: [f64; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDerivedCurve {
    pub name: String,
    pub script: String,
    #[serde(default = "default_scale")]
    pub gain: f32,
    #[serde(default)]
    pub offset: f32,
    #[serde(default)]
    pub visible: bool,
    #[serde(default)]
    pub pane: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAnalysis {
    #[serde(default = "default_harmonic_base")]
    pub harmonic_base_hz: f64,
    #[serde(default = "default_measurement_cycles")]
    pub live_measurement_cycles: f64,
    #[serde(default)]
    pub fft_channel: Option<ProjectChannelRef>,
    #[serde(default)]
    pub sequence_channels: Vec<ProjectChannelRef>,
    #[serde(default)]
    pub pll_channels: Vec<ProjectChannelRef>,
    #[serde(default)]
    pub dq_channels: Vec<ProjectChannelRef>,
    #[serde(default)]
    pub power: Option<ProjectPowerBindings>,
    #[serde(default)]
    pub derived_curves: Vec<ProjectDerivedCurve>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProjectCompareAlignment {
    ManualOffset {
        seconds: f64,
    },
    Anchor {
        reference_time: f64,
        test_time: f64,
    },
    TriggerPoint {
        reference_time: f64,
        test_time: f64,
        confidence: f64,
    },
    ThresholdEvent {
        reference_time: f64,
        test_time: f64,
        confidence: f64,
    },
    FundamentalPhase {
        reference_phase_radians: f64,
        test_phase_radians: f64,
        period_seconds: f64,
        confidence: f64,
    },
}

impl Default for ProjectCompareAlignment {
    fn default() -> Self {
        Self::ManualOffset { seconds: 0.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCompareTolerance {
    #[serde(default)]
    pub absolute: Option<f64>,
    #[serde(default)]
    pub relative: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCompareChannelMapping {
    pub reference: ProjectChannelRef,
    pub test: ProjectChannelRef,
    #[serde(default)]
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCompare {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub reference_dataset_id: Option<DatasetId>,
    #[serde(default)]
    pub test_dataset_id: Option<DatasetId>,
    #[serde(default)]
    pub channel_mappings: Vec<ProjectCompareChannelMapping>,
    #[serde(default)]
    pub alignment: ProjectCompareAlignment,
    #[serde(default)]
    pub tolerance: ProjectCompareTolerance,
    #[serde(default = "default_relative_floor")]
    pub relative_floor: f64,
}

impl Default for ProjectCompare {
    fn default() -> Self {
        Self {
            enabled: false,
            reference_dataset_id: None,
            test_dataset_id: None,
            channel_mappings: Vec::new(),
            alignment: ProjectCompareAlignment::default(),
            tolerance: ProjectCompareTolerance::default(),
            relative_floor: default_relative_floor(),
        }
    }
}

impl Default for ProjectAnalysis {
    fn default() -> Self {
        Self {
            harmonic_base_hz: default_harmonic_base(),
            live_measurement_cycles: default_measurement_cycles(),
            fft_channel: None,
            sequence_channels: Vec::new(),
            pll_channels: Vec::new(),
            dq_channels: Vec::new(),
            power: None,
            derived_curves: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ProjectTransport {
    Tcp { address: String },
    Serial { port: String, baud: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectTriggerMode {
    Auto,
    Normal,
    Single,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectTriggerEdge {
    Rising,
    Falling,
    Either,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTriggerConfig {
    pub mode: ProjectTriggerMode,
    pub edge: ProjectTriggerEdge,
    pub source_channel: u16,
    pub level: f32,
    pub hysteresis: f32,
    pub pre_samples: usize,
    pub post_samples: usize,
    pub auto_timeout_samples: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectLiveProfile {
    #[serde(default)]
    pub transport: Option<ProjectTransport>,
    pub sample_rate_hz: u32,
    pub batch_samples: u16,
    #[serde(default)]
    pub channel_ids: Vec<u16>,
    pub trigger: ProjectTriggerConfig,
    #[serde(default = "default_history_entries")]
    pub capture_history_entries: usize,
    #[serde(default = "default_history_bytes")]
    pub capture_history_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectCaptureOrigin {
    RecordingTrigger,
    CaptureAsset,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCaptureRef {
    pub id: CaptureId,
    pub origin: ProjectCaptureOrigin,
    pub source_id: SourceId,
    #[serde(default)]
    pub trigger_ordinal: Option<usize>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectAnnotationKind {
    Text,
    Arrow,
    Rectangle,
    Ink,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAnnotation {
    pub kind: ProjectAnnotationKind,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub points: Vec<[f32; 2]>,
    pub color: [u8; 4],
    #[serde(default = "default_line_width")]
    pub width: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectExportState {
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub dpi: u32,
    #[serde(default)]
    pub include_cursor_table: bool,
    #[serde(default)]
    pub canvas_width: usize,
    #[serde(default)]
    pub canvas_height: usize,
    #[serde(default)]
    pub annotations: Vec<ProjectAnnotation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeProjectDocument {
    pub scope_project_type: String,
    pub schema_version: u32,
    pub created_by_version: String,
    pub project_id: ProjectId,
    #[serde(default)]
    pub sources: Vec<ProjectSource>,
    #[serde(default)]
    pub datasets: Vec<ProjectDataset>,
    #[serde(default)]
    pub workspace: ProjectWorkspace,
    #[serde(default)]
    pub analysis: ProjectAnalysis,
    #[serde(default)]
    pub compare: ProjectCompare,
    #[serde(default)]
    pub live_profile: Option<ProjectLiveProfile>,
    #[serde(default)]
    pub captures: Vec<ProjectCaptureRef>,
    #[serde(default)]
    pub export: ProjectExportState,
}

impl ScopeProjectDocument {
    pub fn empty(project_id: ProjectId, created_by_version: impl Into<String>) -> Self {
        Self {
            scope_project_type: PROJECT_TYPE.to_owned(),
            schema_version: PROJECT_SCHEMA_VERSION,
            created_by_version: created_by_version.into(),
            project_id,
            sources: Vec::new(),
            datasets: Vec::new(),
            workspace: ProjectWorkspace::default(),
            analysis: ProjectAnalysis::default(),
            compare: ProjectCompare::default(),
            live_profile: None,
            captures: Vec::new(),
            export: ProjectExportState::default(),
        }
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ProjectError> {
        if bytes.len() > MAX_PROJECT_JSON_BYTES {
            return Err(ProjectError::TooLarge(bytes.len()));
        }
        let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
        let schema_version = value
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        if schema_version == Some(LEGACY_PROJECT_SCHEMA_VERSION) {
            let object = value.as_object_mut().ok_or_else(|| {
                ProjectError::InvalidField("project JSON root must be an object".to_owned())
            })?;
            object.insert(
                "schemaVersion".to_owned(),
                serde_json::Value::from(PROJECT_SCHEMA_VERSION),
            );
            object
                .entry("compare".to_owned())
                .or_insert_with(|| serde_json::json!({}));
        }
        if let Some(version) = schema_version {
            if version != LEGACY_PROJECT_SCHEMA_VERSION && version != PROJECT_SCHEMA_VERSION {
                return Err(ProjectError::UnsupportedSchema(version));
            }
        }
        let document: Self = serde_json::from_value(value)?;
        document.validate()?;
        Ok(document)
    }

    pub fn to_pretty_json(&self) -> Result<Vec<u8>, ProjectError> {
        self.validate()?;
        Ok(serde_json::to_vec_pretty(self)?)
    }

    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.scope_project_type != PROJECT_TYPE {
            return Err(ProjectError::WrongType(self.scope_project_type.clone()));
        }
        if self.schema_version != PROJECT_SCHEMA_VERSION {
            return Err(ProjectError::UnsupportedSchema(self.schema_version));
        }
        validate_id("project", &self.project_id.0)?;
        if self.created_by_version.trim().is_empty() {
            return Err(ProjectError::InvalidField(
                "createdByVersion must not be empty".to_owned(),
            ));
        }

        let mut source_ids = HashSet::new();
        for source in &self.sources {
            validate_id("source", &source.id.0)?;
            if !source_ids.insert(source.id.clone()) {
                return Err(ProjectError::DuplicateId(source.id.0.clone()));
            }
            validate_project_path(&source.file.relative_path)?;
        }

        let mut dataset_ids = HashSet::new();
        let mut primary_count = 0_usize;
        for dataset in &self.datasets {
            validate_id("dataset", &dataset.id.0)?;
            if !dataset_ids.insert(dataset.id.clone()) {
                return Err(ProjectError::DuplicateId(dataset.id.0.clone()));
            }
            if !source_ids.contains(&dataset.source_id) {
                return Err(ProjectError::DanglingReference(dataset.source_id.0.clone()));
            }
            if dataset.role == ProjectDatasetRole::Primary {
                primary_count += 1;
            }
            if !dataset.time_offset.is_finite() {
                return Err(ProjectError::InvalidField(
                    "dataset timeOffset must be finite".to_owned(),
                ));
            }
            for channel in &dataset.channels {
                validate_channel_ref(&channel.channel, &source_ids)?;
                if !channel.scale.is_finite() || channel.scale == 0.0 {
                    return Err(ProjectError::InvalidField(
                        "channel scale must be finite and non-zero".to_owned(),
                    ));
                }
                if !channel.line_width.is_finite() || channel.line_width <= 0.0 {
                    return Err(ProjectError::InvalidField(
                        "channel lineWidth must be positive".to_owned(),
                    ));
                }
            }
        }
        if !self.datasets.is_empty() && primary_count != 1 {
            return Err(ProjectError::InvalidField(
                "a non-empty project must have exactly one primary dataset".to_owned(),
            ));
        }

        validate_workspace(&self.workspace, &dataset_ids)?;
        validate_analysis(&self.analysis, &source_ids)?;
        validate_compare(&self.compare, &dataset_ids, &source_ids)?;
        if let Some(profile) = &self.live_profile {
            validate_live_profile(profile)?;
        }

        let mut capture_ids = HashSet::new();
        let mut selected_captures = 0_usize;
        for capture in &self.captures {
            validate_id("capture", &capture.id.0)?;
            if !capture_ids.insert(capture.id.clone()) {
                return Err(ProjectError::DuplicateId(capture.id.0.clone()));
            }
            if !source_ids.contains(&capture.source_id) {
                return Err(ProjectError::DanglingReference(capture.source_id.0.clone()));
            }
            selected_captures += usize::from(capture.selected);
        }
        if selected_captures > 1 {
            return Err(ProjectError::InvalidField(
                "at most one Capture may be selected".to_owned(),
            ));
        }
        for annotation in &self.export.annotations {
            if annotation
                .points
                .iter()
                .flatten()
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
            {
                return Err(ProjectError::InvalidField(
                    "annotation points must be normalized finite values".to_owned(),
                ));
            }
            if !annotation.width.is_finite() || annotation.width <= 0.0 {
                return Err(ProjectError::InvalidField(
                    "annotation width must be positive".to_owned(),
                ));
            }
        }
        if !self.export.annotations.is_empty()
            && (self.export.canvas_width == 0 || self.export.canvas_height == 0)
        {
            return Err(ProjectError::InvalidField(
                "annotation canvas dimensions must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}

pub fn load_project(path: &Path) -> Result<ScopeProjectDocument, ProjectError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PROJECT_JSON_BYTES as u64 {
        return Err(ProjectError::TooLarge(
            usize::try_from(metadata.len()).unwrap_or(usize::MAX),
        ));
    }
    ScopeProjectDocument::from_json_bytes(&fs::read(path)?)
}

pub fn save_project_atomic(
    path: &Path,
    document: &ScopeProjectDocument,
) -> Result<(), ProjectError> {
    let bytes = document.to_pretty_json()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project.scopeproj");
    let temporary = parent.join(format!(".{file_name}.tmp"));
    let backup = parent.join(format!(".{file_name}.bak"));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if path.exists() {
            let _ = fs::remove_file(&backup);
            fs::rename(path, &backup)?;
            if let Err(error) = fs::rename(&temporary, path) {
                let _ = fs::rename(&backup, path);
                return Err(error);
            }
            fs::remove_file(&backup)?;
        } else {
            fs::rename(&temporary, path)?;
        }
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        if path.exists() {
            let _ = fs::remove_file(&backup);
        }
        return Err(ProjectError::Io(error));
    }
    Ok(())
}

pub fn resolve_project_sources(
    project_path: &Path,
    document: &ScopeProjectDocument,
) -> Vec<ResolvedProjectSource> {
    let project_dir = project_path.parent().unwrap_or_else(|| Path::new("."));
    document
        .sources
        .iter()
        .map(|source| {
            let relative = project_dir.join(&source.file.relative_path);
            let candidate = if relative.is_file() {
                Some(relative)
            } else if !source.file.absolute_hint.is_empty() {
                let hint = PathBuf::from(&source.file.absolute_hint);
                hint.is_file().then_some(hint)
            } else {
                None
            };
            let resolution = match candidate {
                Some(path) if file_metadata_matches(&path, &source.file) => {
                    ProjectSourceResolution::Resolved(path)
                }
                Some(path) => ProjectSourceResolution::MetadataMismatch(path),
                None => ProjectSourceResolution::Missing,
            };
            ResolvedProjectSource {
                source_id: source.id.clone(),
                resolution,
            }
        })
        .collect()
}

pub fn relocate_project_source(
    project_path: &Path,
    document: &mut ScopeProjectDocument,
    source_id: &SourceId,
    replacement: &Path,
) -> Result<(), ProjectError> {
    if !replacement.is_file() {
        return Err(ProjectError::InvalidField(format!(
            "replacement source does not exist: {}",
            replacement.display()
        )));
    }
    let source = document
        .sources
        .iter_mut()
        .find(|source| &source.id == source_id)
        .ok_or_else(|| ProjectError::DanglingReference(source_id.0.clone()))?;
    let project_dir = project_path.parent().unwrap_or_else(|| Path::new("."));
    source.file.relative_path = replacement
        .strip_prefix(project_dir)
        .ok()
        .filter(|relative| {
            !relative
                .components()
                .any(|component| component == Component::ParentDir)
        })
        .and_then(Path::to_str)
        .map(str::to_owned)
        .or_else(|| {
            replacement
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
        })
        .ok_or_else(|| ProjectError::UnsafePath(replacement.display().to_string()))?;
    source.file.absolute_hint = replacement.to_string_lossy().into_owned();
    let metadata = fs::metadata(replacement)?;
    source.file.size_bytes = metadata.len();
    source.file.modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok());
    document.validate()
}

fn file_metadata_matches(path: &Path, expected: &ProjectFileRef) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let size_matches = expected.size_bytes == 0 || expected.size_bytes == metadata.len();
    let modified_matches = match expected.modified_unix_ms {
        None => true,
        Some(expected_modified) => {
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|duration| u64::try_from(duration.as_millis()).ok())
                == Some(expected_modified)
        }
    };
    size_matches && modified_matches
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("project JSON exceeds the {MAX_PROJECT_JSON_BYTES}-byte limit: {0}")]
    TooLarge(usize),
    #[error("project JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("project I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a Scope Analyzer project: {0}")]
    WrongType(String),
    #[error("unsupported project schema version {0}")]
    UnsupportedSchema(u32),
    #[error("invalid project identifier: {0}")]
    InvalidId(String),
    #[error("duplicate project identifier: {0}")]
    DuplicateId(String),
    #[error("dangling project reference: {0}")]
    DanglingReference(String),
    #[error("unsafe project-relative path: {0}")]
    UnsafePath(String),
    #[error("invalid project field: {0}")]
    InvalidField(String),
}

fn validate_id(label: &str, value: &str) -> Result<(), ProjectError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err(ProjectError::InvalidId(format!("{label}:{value}")));
    }
    Ok(())
}

fn validate_project_path(value: &str) -> Result<(), ProjectError> {
    let path = Path::new(value);
    let has_windows_prefix =
        value.as_bytes().get(1) == Some(&b':') && value.as_bytes()[0].is_ascii_alphabetic();
    if value.is_empty()
        || path.is_absolute()
        || has_windows_prefix
        || value.starts_with('\\')
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ProjectError::UnsafePath(value.to_owned()));
    }
    Ok(())
}

fn validate_channel_ref(
    channel: &ProjectChannelRef,
    source_ids: &HashSet<SourceId>,
) -> Result<(), ProjectError> {
    if !source_ids.contains(&channel.source_id) {
        return Err(ProjectError::DanglingReference(channel.source_id.0.clone()));
    }
    if channel.raw_name.trim().is_empty() {
        return Err(ProjectError::InvalidField(
            "channel rawName must not be empty".to_owned(),
        ));
    }
    Ok(())
}

fn validate_workspace(
    workspace: &ProjectWorkspace,
    dataset_ids: &HashSet<DatasetId>,
) -> Result<(), ProjectError> {
    if workspace.layout_rows == 0 || workspace.layout_cols == 0 {
        return Err(ProjectError::InvalidField(
            "workspace layout axes must be non-zero".to_owned(),
        ));
    }
    if let Some(selected) = &workspace.selected_dataset_id {
        if !dataset_ids.contains(selected) {
            return Err(ProjectError::DanglingReference(selected.0.clone()));
        }
    }
    let viewport = &workspace.viewport;
    let finite = [
        viewport.view_start,
        viewport.view_end,
        viewport.y_min,
        viewport.y_max,
        viewport.cursor_a,
        viewport.cursor_b,
    ]
    .into_iter()
    .all(f64::is_finite);
    if !finite || viewport.view_end <= viewport.view_start || viewport.y_max <= viewport.y_min {
        return Err(ProjectError::InvalidField(
            "workspace viewport bounds are invalid".to_owned(),
        ));
    }
    if viewport
        .pane_y_bounds
        .iter()
        .any(|bounds| !bounds[0].is_finite() || !bounds[1].is_finite() || bounds[1] <= bounds[0])
    {
        return Err(ProjectError::InvalidField(
            "pane Y bounds are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_analysis(
    analysis: &ProjectAnalysis,
    source_ids: &HashSet<SourceId>,
) -> Result<(), ProjectError> {
    if !analysis.harmonic_base_hz.is_finite() || analysis.harmonic_base_hz <= 0.0 {
        return Err(ProjectError::InvalidField(
            "harmonicBaseHz must be positive".to_owned(),
        ));
    }
    if !analysis.live_measurement_cycles.is_finite()
        || !(1.0..=100.0).contains(&analysis.live_measurement_cycles)
    {
        return Err(ProjectError::InvalidField(
            "liveMeasurementCycles must be in 1..=100".to_owned(),
        ));
    }
    let references = analysis
        .fft_channel
        .iter()
        .chain(&analysis.sequence_channels)
        .chain(&analysis.pll_channels)
        .chain(&analysis.dq_channels);
    for channel in references {
        validate_channel_ref(channel, source_ids)?;
    }
    if let Some(power) = &analysis.power {
        let mut unique = HashSet::new();
        for channel in power.voltage.iter().chain(&power.current) {
            validate_channel_ref(channel, source_ids)?;
            if !unique.insert(channel.clone()) {
                return Err(ProjectError::InvalidField(
                    "power bindings must use six distinct channels".to_owned(),
                ));
            }
        }
        if power
            .voltage_scales
            .iter()
            .chain(&power.current_scales)
            .any(|scale| !scale.is_finite() || *scale == 0.0)
        {
            return Err(ProjectError::InvalidField(
                "power scales must be finite and non-zero".to_owned(),
            ));
        }
    }
    for curve in &analysis.derived_curves {
        if curve.name.trim().is_empty()
            || curve.script.trim().is_empty()
            || !curve.gain.is_finite()
            || !curve.offset.is_finite()
        {
            return Err(ProjectError::InvalidField(
                "derived curve definition is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_compare(
    compare: &ProjectCompare,
    dataset_ids: &HashSet<DatasetId>,
    source_ids: &HashSet<SourceId>,
) -> Result<(), ProjectError> {
    if let Some(reference) = &compare.reference_dataset_id {
        if !dataset_ids.contains(reference) {
            return Err(ProjectError::DanglingReference(reference.0.clone()));
        }
    }
    if let Some(test) = &compare.test_dataset_id {
        if !dataset_ids.contains(test) {
            return Err(ProjectError::DanglingReference(test.0.clone()));
        }
    }
    if compare.enabled {
        let (Some(reference), Some(test)) = (
            compare.reference_dataset_id.as_ref(),
            compare.test_dataset_id.as_ref(),
        ) else {
            return Err(ProjectError::InvalidField(
                "enabled compare requires reference and test datasets".to_owned(),
            ));
        };
        if reference == test {
            return Err(ProjectError::InvalidField(
                "compare reference and test datasets must differ".to_owned(),
            ));
        }
    }
    match compare.alignment {
        ProjectCompareAlignment::ManualOffset { seconds } => {
            if !seconds.is_finite() {
                return Err(ProjectError::InvalidField(
                    "compare alignment offset must be finite".to_owned(),
                ));
            }
        }
        ProjectCompareAlignment::Anchor {
            reference_time,
            test_time,
        } => {
            if !reference_time.is_finite() || !test_time.is_finite() {
                return Err(ProjectError::InvalidField(
                    "compare alignment anchor must be finite".to_owned(),
                ));
            }
        }
        ProjectCompareAlignment::TriggerPoint {
            reference_time,
            test_time,
            confidence,
        }
        | ProjectCompareAlignment::ThresholdEvent {
            reference_time,
            test_time,
            confidence,
        } => {
            if !reference_time.is_finite()
                || !test_time.is_finite()
                || !confidence.is_finite()
                || !(0.0..=1.0).contains(&confidence)
            {
                return Err(ProjectError::InvalidField(
                    "compare event alignment must contain finite times and confidence 0..=1"
                        .to_owned(),
                ));
            }
        }
        ProjectCompareAlignment::FundamentalPhase {
            reference_phase_radians,
            test_phase_radians,
            period_seconds,
            confidence,
        } => {
            if !reference_phase_radians.is_finite()
                || !test_phase_radians.is_finite()
                || !period_seconds.is_finite()
                || period_seconds <= 0.0
                || !confidence.is_finite()
                || !(0.0..=1.0).contains(&confidence)
            {
                return Err(ProjectError::InvalidField(
                    "compare phase alignment must contain finite phases, positive period and confidence 0..=1"
                        .to_owned(),
                ));
            }
        }
    }
    if !compare.relative_floor.is_finite() || compare.relative_floor <= 0.0 {
        return Err(ProjectError::InvalidField(
            "compare relativeFloor must be positive".to_owned(),
        ));
    }
    let tolerance_values = [compare.tolerance.absolute, compare.tolerance.relative];
    if tolerance_values
        .into_iter()
        .flatten()
        .any(|value| !value.is_finite() || value < 0.0)
    {
        return Err(ProjectError::InvalidField(
            "compare tolerance values must be finite and non-negative".to_owned(),
        ));
    }
    for mapping in &compare.channel_mappings {
        validate_channel_ref(&mapping.reference, source_ids)?;
        validate_channel_ref(&mapping.test, source_ids)?;
        if mapping.label.trim().is_empty() {
            return Err(ProjectError::InvalidField(
                "compare channel mapping label must not be empty".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_live_profile(profile: &ProjectLiveProfile) -> Result<(), ProjectError> {
    if profile.sample_rate_hz == 0 || profile.batch_samples == 0 {
        return Err(ProjectError::InvalidField(
            "Live acquisition rate and batch must be non-zero".to_owned(),
        ));
    }
    if profile.capture_history_entries == 0 || profile.capture_history_entries > 10_000 {
        return Err(ProjectError::InvalidField(
            "Capture history entry limit is invalid".to_owned(),
        ));
    }
    if profile.capture_history_bytes == 0 || profile.capture_history_bytes > 4 * 1024 * 1024 * 1024
    {
        return Err(ProjectError::InvalidField(
            "Capture history byte limit is invalid".to_owned(),
        ));
    }
    if !profile.trigger.level.is_finite()
        || !profile.trigger.hysteresis.is_finite()
        || profile.trigger.hysteresis < 0.0
    {
        return Err(ProjectError::InvalidField(
            "trigger level/hysteresis is invalid".to_owned(),
        ));
    }
    if let Some(ProjectTransport::Serial { port, baud }) = &profile.transport {
        if port.trim().is_empty() || *baud == 0 {
            return Err(ProjectError::InvalidField(
                "serial transport preset is invalid".to_owned(),
            ));
        }
    }
    if let Some(ProjectTransport::Tcp { address }) = &profile.transport {
        if address.trim().is_empty() {
            return Err(ProjectError::InvalidField(
                "TCP transport preset is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

const fn default_true() -> bool {
    true
}

const fn default_layout_axis() -> usize {
    1
}

const fn default_scale() -> f32 {
    1.0
}

const fn default_line_width() -> f32 {
    1.5
}

const fn default_harmonic_base() -> f64 {
    50.0
}

const fn default_measurement_cycles() -> f64 {
    10.0
}

const fn default_scales() -> [f64; 3] {
    [1.0; 3]
}

const fn default_history_entries() -> usize {
    100
}

const fn default_history_bytes() -> u64 {
    128 * 1024 * 1024
}

const fn default_relative_floor() -> f64 {
    1.0e-12
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "scope-project-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn source() -> ProjectSource {
        ProjectSource {
            id: SourceId("source-main".to_owned()),
            kind: ProjectSourceKind::Csv,
            file: ProjectFileRef {
                relative_path: "data/main.csv".to_owned(),
                absolute_hint: String::new(),
                size_bytes: 123,
                modified_unix_ms: None,
                partial_crc32c: None,
            },
            required: true,
        }
    }

    fn channel(index: usize, name: &str) -> ProjectChannelRef {
        ProjectChannelRef {
            source_id: SourceId("source-main".to_owned()),
            raw_name: name.to_owned(),
            index_hint: index,
            channel_id_hint: None,
        }
    }

    fn document() -> ScopeProjectDocument {
        let mut document =
            ScopeProjectDocument::empty(ProjectId("project-test".to_owned()), "0.11.0-test");
        document.sources.push(source());
        document.datasets.push(ProjectDataset {
            id: DatasetId("dataset-main".to_owned()),
            source_id: SourceId("source-main".to_owned()),
            role: ProjectDatasetRole::Primary,
            display_name: "Main".to_owned(),
            enabled: true,
            line_pattern: "solid".to_owned(),
            time_offset: 0.0,
            channels: Vec::new(),
        });
        document.workspace.selected_dataset_id = Some(DatasetId("dataset-main".to_owned()));
        document.analysis.fft_channel = Some(channel(0, "Va"));
        document
    }

    #[test]
    fn project_v1_round_trips_with_type_and_schema_validation() {
        let document = document();
        let bytes = document.to_pretty_json().unwrap();
        let decoded = ScopeProjectDocument::from_json_bytes(&bytes).unwrap();
        assert_eq!(decoded, document);
        assert!(String::from_utf8(bytes)
            .unwrap()
            .contains("\"schemaVersion\": 2"));
    }

    #[test]
    fn project_v1_json_migrates_to_v2_with_disabled_compare() {
        let document = document();
        let mut value = serde_json::to_value(document).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("schemaVersion".to_owned(), serde_json::json!(1));
        object.remove("compare");

        let migrated =
            ScopeProjectDocument::from_json_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();

        assert_eq!(migrated.schema_version, 2);
        assert!(!migrated.compare.enabled);
        assert_eq!(migrated.compare.reference_dataset_id, None);
    }

    #[test]
    fn compatibility_fixture_migrates_explicitly_to_v2() {
        let migrated = ScopeProjectDocument::from_json_bytes(include_bytes!(
            "../tests/fixtures/compatibility/scopeproj-v1-minimal.json"
        ))
        .unwrap();
        assert_eq!(migrated.schema_version, PROJECT_SCHEMA_VERSION);
        assert!(!migrated.compare.enabled);
        assert_eq!(migrated.datasets.len(), 1);
    }

    #[test]
    #[ignore = "external-input fuzz gate; run explicitly in the release job"]
    fn project_parser_survives_one_million_deterministic_inputs() {
        let mut state = 0x243f_6a88_u32;
        for length in 0..1_000_000_usize {
            let mut bytes = vec![0_u8; length % 96];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = (state & 0xff) as u8;
            }
            let _ = ScopeProjectDocument::from_json_bytes(&bytes);
        }
    }

    #[test]
    fn project_compare_validation_rejects_invalid_dataset_and_tolerance() {
        let mut document = document();
        document.compare.enabled = true;
        document.compare.reference_dataset_id = Some(DatasetId("dataset-main".to_owned()));
        document.compare.test_dataset_id = Some(DatasetId("missing".to_owned()));
        assert!(matches!(
            document.validate(),
            Err(ProjectError::DanglingReference(_))
        ));

        document.compare.test_dataset_id = Some(DatasetId("dataset-main".to_owned()));
        document.compare.tolerance.absolute = Some(-1.0);
        assert!(matches!(
            document.validate(),
            Err(ProjectError::InvalidField(_))
        ));
    }

    #[test]
    fn project_compare_validation_accepts_event_and_phase_alignment() {
        let mut document = document();
        let mut imported_source = source();
        imported_source.id = SourceId("source-imported".to_owned());
        imported_source.file.relative_path = "data/imported.csv".to_owned();
        document.sources.push(imported_source);
        document.datasets.push(ProjectDataset {
            id: DatasetId("dataset-imported".to_owned()),
            source_id: SourceId("source-imported".to_owned()),
            role: ProjectDatasetRole::Imported,
            display_name: "Imported".to_owned(),
            enabled: true,
            line_pattern: "dashed".to_owned(),
            time_offset: 0.0,
            channels: Vec::new(),
        });
        document.compare.enabled = true;
        document.compare.reference_dataset_id = Some(DatasetId("dataset-main".to_owned()));
        document.compare.test_dataset_id = Some(DatasetId("dataset-imported".to_owned()));
        document.compare.alignment = ProjectCompareAlignment::TriggerPoint {
            reference_time: 1.0,
            test_time: 0.8,
            confidence: 0.9,
        };
        assert!(document.validate().is_ok());

        document.compare.alignment = ProjectCompareAlignment::FundamentalPhase {
            reference_phase_radians: 0.25,
            test_phase_radians: -0.25,
            period_seconds: 0.02,
            confidence: 0.8,
        };
        assert!(document.validate().is_ok());
    }

    #[test]
    fn project_compare_validation_rejects_invalid_alignment_confidence() {
        let mut document = document();
        document.compare.alignment = ProjectCompareAlignment::ThresholdEvent {
            reference_time: 1.0,
            test_time: 0.8,
            confidence: 1.1,
        };
        assert!(matches!(
            document.validate(),
            Err(ProjectError::InvalidField(_))
        ));
    }

    #[test]
    fn project_rejects_duplicate_and_dangling_ids() {
        let mut duplicate = document();
        duplicate.sources.push(source());
        assert!(matches!(
            duplicate.validate(),
            Err(ProjectError::DuplicateId(_))
        ));

        let mut dangling = document();
        dangling.datasets[0].source_id = SourceId("missing".to_owned());
        assert!(matches!(
            dangling.validate(),
            Err(ProjectError::DanglingReference(_))
        ));
    }

    #[test]
    fn project_rejects_parent_or_absolute_relative_paths() {
        for unsafe_path in [
            "../secret.csv",
            "/tmp/data.csv",
            r"C:\data\main.csv",
            r"\\server\share\main.csv",
        ] {
            let mut document = document();
            document.sources[0].file.relative_path = unsafe_path.to_owned();
            assert!(matches!(
                document.validate(),
                Err(ProjectError::UnsafePath(_))
            ));
        }
    }

    #[test]
    fn project_rejects_non_finite_viewport_without_mutating_input() {
        let mut document = document();
        document.workspace.viewport.view_end = f64::NAN;
        assert!(matches!(
            document.validate(),
            Err(ProjectError::InvalidField(_))
        ));
        assert!(document.workspace.viewport.view_end.is_nan());
    }

    #[test]
    fn project_rejects_oversized_json_before_parsing() {
        let bytes = vec![b' '; MAX_PROJECT_JSON_BYTES + 1];
        assert!(matches!(
            ScopeProjectDocument::from_json_bytes(&bytes),
            Err(ProjectError::TooLarge(_))
        ));
    }

    #[test]
    fn project_requires_exactly_one_primary_dataset() {
        let mut document = document();
        document.datasets[0].role = ProjectDatasetRole::Imported;
        assert!(matches!(
            document.validate(),
            Err(ProjectError::InvalidField(_))
        ));
    }

    #[test]
    fn project_atomic_save_load_and_source_resolution_round_trip() {
        let directory = temporary_dir("atomic");
        let data_path = directory.join("data/main.csv");
        fs::create_dir_all(data_path.parent().unwrap()).unwrap();
        fs::write(&data_path, b"time,value\n0,1\n").unwrap();
        let mut document = document();
        document.sources[0].file.size_bytes = fs::metadata(&data_path).unwrap().len();
        let project_path = directory.join("test.scopeproj");

        save_project_atomic(&project_path, &document).unwrap();
        assert_eq!(load_project(&project_path).unwrap(), document);
        document.datasets[0].display_name = "Updated".to_owned();
        save_project_atomic(&project_path, &document).unwrap();
        assert_eq!(load_project(&project_path).unwrap(), document);
        assert!(matches!(
            resolve_project_sources(&project_path, &document)[0].resolution,
            ProjectSourceResolution::Resolved(_)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn relocation_updates_hint_and_detects_metadata_mismatch() {
        let directory = temporary_dir("relocate");
        let replacement = directory.join("replacement.csv");
        fs::write(&replacement, b"replacement").unwrap();
        let project_path = directory.join("test.scopeproj");
        let mut document = document();

        relocate_project_source(
            &project_path,
            &mut document,
            &SourceId("source-main".to_owned()),
            &replacement,
        )
        .unwrap();
        assert_eq!(document.sources[0].file.relative_path, "replacement.csv");
        fs::write(&replacement, b"changed-size").unwrap();
        assert!(matches!(
            resolve_project_sources(&project_path, &document)[0].resolution,
            ProjectSourceResolution::MetadataMismatch(_)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn source_resolution_rejects_same_size_file_with_different_mtime() {
        let directory = temporary_dir("same-size-mtime");
        let data_path = directory.join("data/main.csv");
        fs::create_dir_all(data_path.parent().unwrap()).unwrap();
        fs::write(&data_path, b"time,value\n0,1\n").unwrap();
        let original_metadata = fs::metadata(&data_path).unwrap();
        let original_modified = original_metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .try_into()
            .unwrap();
        let mut document = document();
        document.sources[0].file.size_bytes = original_metadata.len();
        document.sources[0].file.modified_unix_ms = Some(original_modified);
        let project_path = directory.join("test.scopeproj");

        fs::write(&data_path, b"time,value\n0,2\n").unwrap();
        let replacement_modified = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(original_modified)
            + std::time::Duration::from_secs(1);
        fs::OpenOptions::new()
            .write(true)
            .open(&data_path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(replacement_modified))
            .unwrap();

        assert_eq!(
            fs::metadata(&data_path).unwrap().len(),
            original_metadata.len()
        );
        assert!(matches!(
            resolve_project_sources(&project_path, &document)[0].resolution,
            ProjectSourceResolution::MetadataMismatch(_)
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn project_validation_covers_rejection_matrix_for_persisted_sections() {
        let mut invalid = document();
        invalid.scope_project_type = "other".to_owned();
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::WrongType(_))
        ));

        let mut invalid = document();
        invalid.schema_version = 99;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::UnsupportedSchema(99))
        ));

        let mut invalid = document();
        invalid.created_by_version.clear();
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        for value in [String::new(), "not valid".to_owned(), "x".repeat(129)] {
            let mut invalid = document();
            invalid.project_id = ProjectId(value);
            assert!(matches!(
                invalid.validate(),
                Err(ProjectError::InvalidId(_))
            ));
        }

        let mut invalid = document();
        invalid.sources[0].id = SourceId("bad id".to_owned());
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidId(_))
        ));

        let mut invalid = document();
        invalid.datasets[0].time_offset = f64::NAN;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.datasets[0].channels.push(ProjectChannelState {
            channel: channel(0, "Va"),
            display_name: String::new(),
            color: [0, 0, 0, 255],
            visible: true,
            scale: 0.0,
            pane: 0,
            line_width: 1.0,
            line_pattern: String::new(),
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.datasets[0].channels.push(ProjectChannelState {
            channel: channel(0, "Va"),
            display_name: String::new(),
            color: [0, 0, 0, 255],
            visible: true,
            scale: 1.0,
            pane: 0,
            line_width: 0.0,
            line_pattern: String::new(),
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.workspace.layout_rows = 0;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.workspace.selected_dataset_id = Some(DatasetId("missing".to_owned()));
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::DanglingReference(_))
        ));

        let mut invalid = document();
        invalid.workspace.viewport.view_end = invalid.workspace.viewport.view_start;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.workspace.viewport.pane_y_bounds = vec![[1.0, 1.0]];
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.analysis.harmonic_base_hz = 0.0;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.analysis.live_measurement_cycles = 101.0;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.analysis.fft_channel = Some(channel(0, ""));
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let refs = [
            channel(0, "V1"),
            channel(1, "V2"),
            channel(2, "V3"),
            channel(3, "I1"),
            channel(4, "I2"),
        ];
        let mut invalid = document();
        invalid.analysis.power = Some(ProjectPowerBindings {
            voltage: [refs[0].clone(), refs[1].clone(), refs[2].clone()],
            current: [refs[3].clone(), refs[3].clone(), refs[4].clone()],
            voltage_scales: [1.0; 3],
            current_scales: [1.0; 3],
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.analysis.power = Some(ProjectPowerBindings {
            voltage: [refs[0].clone(), refs[1].clone(), refs[2].clone()],
            current: [refs[3].clone(), refs[4].clone(), channel(5, "I3")],
            voltage_scales: [0.0, 1.0, 1.0],
            current_scales: [1.0; 3],
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.analysis.derived_curves.push(ProjectDerivedCurve {
            name: String::new(),
            script: "x".to_owned(),
            gain: 1.0,
            offset: 0.0,
            visible: true,
            pane: 0,
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.compare.enabled = true;
        invalid.compare.reference_dataset_id = None;
        invalid.compare.test_dataset_id = None;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.compare.enabled = true;
        invalid.compare.reference_dataset_id = Some(DatasetId("dataset-main".to_owned()));
        invalid.compare.test_dataset_id = Some(DatasetId("dataset-main".to_owned()));
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let invalid_alignments = [
            ProjectCompareAlignment::ManualOffset { seconds: f64::NAN },
            ProjectCompareAlignment::Anchor {
                reference_time: f64::NAN,
                test_time: 0.0,
            },
            ProjectCompareAlignment::TriggerPoint {
                reference_time: 0.0,
                test_time: 0.0,
                confidence: 2.0,
            },
            ProjectCompareAlignment::ThresholdEvent {
                reference_time: 0.0,
                test_time: 0.0,
                confidence: -0.1,
            },
            ProjectCompareAlignment::FundamentalPhase {
                reference_phase_radians: 0.0,
                test_phase_radians: 0.0,
                period_seconds: 0.0,
                confidence: 0.5,
            },
        ];
        for alignment in invalid_alignments {
            let mut invalid = document();
            invalid.compare.alignment = alignment;
            assert!(matches!(
                invalid.validate(),
                Err(ProjectError::InvalidField(_))
            ));
        }

        let mut invalid = document();
        invalid.compare.relative_floor = 0.0;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.compare.tolerance.relative = Some(-0.1);
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid
            .compare
            .channel_mappings
            .push(ProjectCompareChannelMapping {
                reference: channel(0, "Va"),
                test: channel(0, "Vb"),
                label: String::new(),
            });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let live = ProjectLiveProfile {
            transport: Some(ProjectTransport::Serial {
                port: String::new(),
                baud: 0,
            }),
            sample_rate_hz: 0,
            batch_samples: 0,
            channel_ids: Vec::new(),
            trigger: ProjectTriggerConfig {
                mode: ProjectTriggerMode::Auto,
                edge: ProjectTriggerEdge::Rising,
                source_channel: 0,
                level: 0.0,
                hysteresis: 0.0,
                pre_samples: 0,
                post_samples: 0,
                auto_timeout_samples: 0,
            },
            capture_history_entries: 0,
            capture_history_bytes: 0,
        };
        let mut invalid = document();
        invalid.live_profile = Some(live);
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.captures = vec![
            ProjectCaptureRef {
                id: CaptureId("capture".to_owned()),
                origin: ProjectCaptureOrigin::CaptureAsset,
                source_id: SourceId("source-main".to_owned()),
                trigger_ordinal: None,
                label: String::new(),
                note: String::new(),
                pinned: false,
                selected: true,
            },
            ProjectCaptureRef {
                id: CaptureId("capture".to_owned()),
                origin: ProjectCaptureOrigin::RecordingTrigger,
                source_id: SourceId("source-main".to_owned()),
                trigger_ordinal: None,
                label: String::new(),
                note: String::new(),
                pinned: false,
                selected: true,
            },
        ];
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::DuplicateId(_))
        ));

        let mut invalid = document();
        invalid.captures.push(ProjectCaptureRef {
            id: CaptureId("capture".to_owned()),
            origin: ProjectCaptureOrigin::RecordingTrigger,
            source_id: SourceId("missing".to_owned()),
            trigger_ordinal: None,
            label: String::new(),
            note: String::new(),
            pinned: false,
            selected: false,
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::DanglingReference(_))
        ));

        let mut invalid = document();
        invalid.captures = (0..2)
            .map(|index| ProjectCaptureRef {
                id: CaptureId(format!("capture-{index}")),
                origin: ProjectCaptureOrigin::RecordingTrigger,
                source_id: SourceId("source-main".to_owned()),
                trigger_ordinal: None,
                label: String::new(),
                note: String::new(),
                pinned: false,
                selected: true,
            })
            .collect();
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.export.annotations.push(ProjectAnnotation {
            kind: ProjectAnnotationKind::Text,
            text: "bad".to_owned(),
            points: vec![[1.1, 0.0]],
            color: [0, 0, 0, 255],
            width: 1.0,
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.export.annotations.push(ProjectAnnotation {
            kind: ProjectAnnotationKind::Arrow,
            text: String::new(),
            points: vec![[0.0, 0.0]],
            color: [0, 0, 0, 255],
            width: 0.0,
        });
        invalid.export.canvas_width = 100;
        invalid.export.canvas_height = 100;
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));

        let mut invalid = document();
        invalid.export.annotations.push(ProjectAnnotation {
            kind: ProjectAnnotationKind::Rectangle,
            text: String::new(),
            points: vec![[0.0, 0.0]],
            color: [0, 0, 0, 255],
            width: 1.0,
        });
        assert!(matches!(
            invalid.validate(),
            Err(ProjectError::InvalidField(_))
        ));
    }
}
