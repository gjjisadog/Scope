# Live Scope Design QA

- source visual truth path: `/Users/wangxuwen/.codex/generated_images/019f501a-582a-7bc1-b4b2-9b6f331ad399/exec-a19468fc-eb91-4270-a355-8168e628089b.png`
- implementation screenshot path: `/Users/wangxuwen/Documents/Scope/design-qa-implementation.png`
- full-view comparison evidence: `/Users/wangxuwen/Documents/Scope/design-qa-comparison.png`
- focused region comparison evidence: `/Users/wangxuwen/Documents/Scope/design-qa-focused.png`
- viewport: 1370 × 768 native macOS window; source direction is 1487 × 1058
- state: dark theme, TCP simulator connected, acquisition streaming, trigger armed, four channels visible, Signals/Inspector/Events docks open, Trigger and Events tabs selected

## Findings

No actionable P0, P1, or P2 differences remain.

- [P3] Native typography is optically lighter and smaller than the concept at the normalized comparison scale.
  - Location: toolbar, signal metadata, inspector labels, and event table.
  - Evidence: the source uses a slightly heavier compact desktop type treatment; the implementation uses the application's existing system-font stack at native egui density.
  - Impact: minor fidelity drift only; labels remain legible at the actual native viewport and no text overlaps or truncates.
  - Follow-up: if the product later adopts dedicated UI tokens, raise secondary text weight/size by one egui text-style step instead of adding screen-specific font overrides.

## Required Fidelity Surfaces

- Fonts and typography: hierarchy, weights, wrapping, and truncation were checked in the focused comparison. Native system-font rendering is retained intentionally for Chinese coverage and consistency with the existing app; the residual optical difference is P3.
- Spacing and layout rhythm: toolbar, document tab, left signal tree, linked center lanes, right inspector, and bottom event/link dock preserve the source hierarchy. Panels remain usable at 1370 × 768 and can collapse or resize.
- Colors and visual tokens: graphite surfaces, low-contrast dividers, teal active/connected states, amber trigger marker, red recording/error semantics, and channel-local colors match the selected direction. Contrast remains readable in the captured dark state.
- Image quality and asset fidelity: the source contains no required illustration, logo, or product-image assets inside the Live workspace. Waveforms and markers are native vector-rendered plots and remain sharp.
- Copy and content: Chinese labels are concise and product-specific. Connected, streaming, trigger, recording, empty, and health states use real application/session values rather than placeholder prose.
- Icons and affordances: the compact toolbar uses the app's existing egui affordances and text labels; active panel buttons and tabs expose state without decorative or fake assets.

## Comparison History

### Iteration 1

- Earlier finding: [P1] after ten seconds of streaming, the two 50 Hz analog channels filled their lanes as solid color bands because the default 10-second history exposed 500 cycles at once. This obscured waveform shape and undermined the primary task.
- Fix made: changed the Live Scope default history window from 10 seconds to 1 second and updated the release note. The history control remains editable for longer trend inspection.
- Post-fix evidence: `design-qa-implementation.png` and both comparison images show distinct sine, saw, and digital edges while preserving an overview-scale recent history window.

### Iteration 2

- Result: no remaining P0/P1/P2 differences in the full-view or focused comparison.

## Primary Interactions Tested

- Enter Live workspace.
- Connect to TCP simulator and transition to Ready.
- Apply configuration and begin streaming.
- Switch Trigger/Display inspector tabs.
- Switch Events/Link bottom tabs.
- Collapse and reopen Signals, Inspector, and bottom docks.
- Observe linked channel lanes and shared trigger cursor during live updates.

## Residual Test Gaps

- The source mock is taller than the available desktop capture, so exact pixel-for-pixel vertical proportions were not judged; responsive structure and control availability were judged at the real native viewport instead.
- Windows-specific font rasterization and packaged renderer behavior were not visually captured on macOS; cross-platform compile/test coverage remains separate from this visual pass.

## Implementation Checklist

- [x] Preserve dominant central waveform workspace.
- [x] Provide grouped searchable signal management.
- [x] Provide compact acquisition controls and clear connection state.
- [x] Provide Trigger/Display/Diagnostics inspector tabs.
- [x] Provide Events/Link bottom tabs.
- [x] Make core panel visibility and tab interactions functional.
- [x] Keep recent live traces readable by default.

## Follow-up Polish

- Consider a product-wide compact typography token pass after validating Windows DPI scaling; do not tune only this screen.

final result: passed
