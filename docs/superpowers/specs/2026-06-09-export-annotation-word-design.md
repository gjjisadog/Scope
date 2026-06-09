# Export Annotation And Word Report Design

## Goal

Rebuild the waveform image export experience into a screenshot-tool-style annotation workspace, then add a Word report export path that can batch insert annotated waveform images into one default report template.

The primary user outcome is: after loading experiment waveform data, the user can select time windows, annotate the exported waveform image with variable labels, arrows, text, and freehand marks, then generate either image files or a Word report without manually screenshotting and rewriting the experiment report.

## Confirmed Product Direction

The selected direction is a screenshot-style annotation workspace:

- A dedicated export preview window with a tool strip, central canvas, property controls, and export actions.
- Automatic variable labels with arrows pointing to their corresponding curves.
- Manual annotation tools for experiment explanations, such as text notes, free arrows, brush, and eraser.
- Batch export for multiple time windows, panes, or datasets.
- Word export that inserts multiple annotated waveform images into one `.docx`.

The selected Word report format is a built-in simple experiment report template:

- Report title and basic metadata at the top.
- One full-width annotated waveform image per section.
- A generated figure caption under each image.
- Cursor data table can be included or omitted by one export option.
- No external Word template import in the first version.

## First-Version Scope

In scope:

- Rework the export preview UI into a more explicit annotation workspace.
- Preserve existing PNG and SVG export behavior where possible.
- Support automatic variable labels and curve-bound arrows.
- Allow variable labels to be dragged.
- Recompute variable arrow length and angle when a variable label moves.
- Allow the arrow tip anchor to move along the same curve by dragging the anchor.
- Support manual text annotations with editable text, color, and size.
- Support manual free arrows that are independent of variables.
- Support brush strokes and eraser.
- Support undo and redo for annotation edits.
- Support batch PNG export using the same annotation settings.
- Add Word report export that writes one `.docx` containing multiple annotated images.
- Add a Word option to include or hide cursor data tables.
- Add tests for annotation state behavior, Word package structure, and existing export rendering.
- Update user-facing documentation.
- Because this is a major user-facing export workflow change, update the application version in the release metadata files required by `AGENTS.md`.

Out of scope for the first version:

- Importing arbitrary external Word templates.
- Editing a Word template inside the application.
- Automatic fault detection, peak detection, transient detection, or semantic experiment explanation.
- OCR, system-level screenshot capture, or annotating arbitrary screen regions outside the exported waveform canvas.
- Multi-user annotation collaboration.

## Current Codebase Context

The current export implementation already includes several useful foundations:

- `src/png_export.rs` provides a raster canvas with lines, arrows, text, rectangles, and PNG saving with DPI metadata.
- `src/svg_export.rs` provides SVG output through the same `WaveformCanvas` trait.
- `src/app.rs` currently owns most export UI state, preview interaction, rendering, batch export, and settings.
- Existing state already includes variable label overrides, label positions, label anchor X positions, text annotations, ink strokes, undo and redo, resolution, DPI, pane scope, and time range settings.

The main technical problem is that export behavior is concentrated in `src/app.rs`. The implementation should split the export annotation model and Word writer into focused modules instead of adding substantially more logic to the existing large file.

## Proposed Architecture

### Annotation Model

Create a focused export annotation model that stores export-time annotations independently from the egui preview widgets.

Core concepts:

- `VariableLabelAnnotation`: generated from an exported curve, bound to one curve identity, with editable text, label position, optional curve anchor X, style, and visibility.
- `TextAnnotation`: user-created text note with position, content, color, and size.
- `ArrowAnnotation`: user-created free arrow with start point, end point, color, width, line style, and optional attached text.
- `InkStroke`: freehand stroke with points, color, and width.
- `AnnotationState`: the complete editable state for the preview, including undo and redo snapshots.

Variable labels remain curve-bound. Dragging a variable label updates only the label position. The renderer recomputes the arrow start from the label rectangle edge and draws the arrow to the curve anchor. Dragging the anchor changes the anchor X value and remaps the arrow tip to the same curve at that X coordinate.

Manual arrows are not curve-bound. They are for experiment observations such as fault points, switching moments, or user-written explanations.

### Export Rendering

Keep one rendering path for preview, PNG, SVG, batch PNG, and Word image generation:

1. Resolve export selection, pane scope, and time range.
2. Build export curves from current selected channels and prepared plot caches.
3. Generate default variable labels for visible curves.
4. Merge user annotation state.
5. Render waveform, cursors, optional cursor table, variable labels, manual text, manual arrows, and ink.
6. Save the rendered canvas as PNG/SVG or pass the PNG bytes/path into the Word writer.

The preview should be a live view of the same rendered output that final files use. This avoids the common mismatch where the preview looks correct but the exported file differs.

### Annotation Workspace UI

The export preview window should be reorganized into:

- Left tool strip: select, text, arrow, brush, eraser, undo, redo.
- Top or compact settings row: pane scope, time range, resolution, DPI, cursor table toggle.
- Central scrollable canvas: rendered waveform image with overlay hit targets for labels, anchors, text, arrows, and strokes.
- Right property panel: selected annotation properties such as text, color, size, line width, arrow style, and delete action.
- Export actions: save PNG, save SVG, export Word report, batch export.

The UI should avoid hidden behavior:

- Variable labels show hover outline and can be dragged.
- Curve-bound arrow anchors show a small handle and can be dragged along the plot area.
- Manual arrows expose handles for both endpoints.
- Double-clicking a label or text annotation opens editing.
- Undo and redo apply to all annotation edits.

### Batch Export

Batch export keeps the current concept of multiple time windows, dataset mode, and pane mode, then extends the output targets.

For PNG:

- Export each selected job to a separate PNG file.
- Continue using deterministic filenames based on base name, window index, time range, dataset, and pane.

For Word:

- Export each selected job to an annotated PNG internally.
- Insert all generated images into one `.docx`.
- Use one report section per generated image.
- Use figure captions that include window index and time range.
- Include cursor data table under each image only when the Word cursor table option is enabled.

The Word export should not require saving intermediate PNG files unless the implementation needs a temporary file. If temporary files are used, they should be cleaned up after export.

### Word Default Template

The first version uses a built-in simple Word template generated by code.

The default document structure:

1. Title: `实验波形报告`.
2. Metadata block:
   - experiment name, defaulting to the dataset or source name;
   - data file name;
   - export time;
   - sample rate when available;
   - time range summary.
3. Repeating figure section:
   - annotated waveform image;
   - caption such as `图 1：窗口 #1 0.100000s - 0.200000s`;
   - optional cursor data table.

The first version does not import external `.docx` templates. The internal writer should still keep report data separate from OpenXML writing so a later external-template feature can reuse the same report data model.

### Data Flow

The expected flow for single-image export:

1. User chooses `Export Waveform Image`.
2. Application validates data is loaded and at least one curve is selected.
3. Application refreshes plot caches if needed.
4. Application opens annotation workspace.
5. User edits annotations.
6. User saves PNG, SVG, or Word.
7. Renderer produces final output from the same annotation state shown in preview.

The expected flow for Word batch export:

1. User opens batch export.
2. User defines time windows, dataset mode, pane mode, and whether cursor data tables should be included.
3. User chooses `Export Word Report`.
4. Application renders each batch item with the shared annotation settings and automatic variable labels.
5. Application writes one `.docx` using the built-in simple template.
6. Application reports how many figures were inserted and any failed windows.

## Error Handling

The implementation should surface clear user-facing errors for:

- no waveform data loaded;
- no selected curves;
- plot data still refreshing;
- invalid time range;
- no enabled batch windows;
- Word output path not writable;
- image generation failure for a batch item;
- Word package writing failure.

For batch Word export, partial failure should be explicit. The first-version behavior is to fail the whole Word export if any figure cannot render, so the generated report is not silently incomplete.

## Persistence And Compatibility

Existing display configuration can keep storing export settings such as resolution, DPI, pane scope, time range mode, arrow size, label scale, and colors.

New annotation state should be treated as export-preview session state by default, not long-term project configuration. The user can recreate automatic variable labels from current selections. Persisting annotations across app restarts is not required in the first version.

Existing config loading should tolerate older config files that do not include any new export fields.

## Testing And Verification

Minimum automated verification:

- Unit tests for label position clamping and variable-arrow recomputation.
- Unit tests for manual arrow state editing and undo/redo snapshots.
- Unit tests for Word writer package structure:
  - document has required OpenXML parts;
  - generated `.docx` contains inserted image relationships;
  - cursor table is present when enabled and absent when disabled.
- Existing export-related tests should continue to pass.

Manual verification:

- Open a waveform file.
- Select multiple curves.
- Open export annotation workspace.
- Drag a variable label and confirm the arrow follows the label while pointing to the curve.
- Drag a variable anchor and confirm the arrow tip follows the same curve.
- Add manual text and a manual arrow.
- Save PNG and confirm final image matches preview.
- Batch export multiple windows to PNG.
- Export one Word report with cursor data tables enabled.
- Export one Word report with cursor data tables disabled.
- Open the generated `.docx` in Word or a compatible viewer and confirm image layout, captions, and optional tables.

## Documentation

Update README and in-app help text to describe:

- the new annotation workspace;
- automatic variable labels;
- manual arrows and text;
- batch Word report export;
- cursor data table toggle;
- first-version limitation that external Word template import is not yet supported.

## Versioning

This change alters user-facing export behavior and introduces a new Word report export workflow. It counts as a major change under the repository rules.

When implementing, update these files in the same change set:

- `Cargo.toml` package `version`;
- `scripts/package-windows.ps1` `$version`;
- `scripts/ScopeAnalyzer.wxs` `Product Version`;
- README package artifact names if they include the version.
