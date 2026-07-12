# Task Plan: Scope Analyzer Next-Stage Product Roadmap

## Goal
Completely scan the current repository after the reliability, recording-consistency, state-machine, performance, and shared Live/offline-analysis work; deliver a product-function inventory, gap analysis grounded in power-electronics and DSP debugging workflows, priority recommendations, a feature matrix, and a versioned roadmap. Do not implement proposed features in this phase.

## Current Phase
Complete — report delivered for user scope selection

## Deliverable Contract
- Inventory every user-requested current capability from source, tests, docs, CLI, and extension evidence.
- Evaluate workflow gaps as product capabilities, not merely refactors, performance work, or visual polish.
- Classify recommendations as P0, P1, P2, or temporarily not recommended.
- For every recommendation include current state, pain, proposed feature, scenario, modules, SCP1/`.scope` format impact, complexity, dependencies, acceptance criteria, and recommended version.
- Include the requested feature matrix and version roadmap.
- Preserve the Scope Analyzer product boundary.
- Use “当前仓库” consistently in the Chinese report.
- Stop after the scan and priority report; wait for user scope selection before implementation planning.

## Phases

### Phase 1: Onboarding and Repository Map
- [x] Confirm runtime, entrypoints, docs, tests, automation surfaces, and local constraints.
- [x] Attempt GitNexus index access; record unavailable MCP and complete architecture map from source/tests/docs.
- **Status:** complete

### Phase 2: Current Capability Inventory
- [x] Verify the 15 requested capability areas against implementation and tests.
- [x] Record support level and important limitations for each.
- **Status:** complete

### Phase 3: Workflow Gap Analysis
- [x] Model representative power-electronics/DSP workflows.
- [x] Evaluate all user-prioritized directions and identify missing closure points.
- [x] Separate product gaps from engineering-only cleanup.
- **Status:** complete

### Phase 4: Prioritization and Versioning
- [x] Classify recommendations into P0/P1/P2/not recommended.
- [x] Define dependencies, format/protocol impacts, complexity, risk, and acceptance criteria.
- [x] Build the feature matrix and version roadmap.
- **Status:** complete

### Phase 5: Report QA and Delivery
- [x] Audit every requested field and terminology constraint.
- [x] Deliver the scan report and wait for scope selection.
- **Status:** complete

## Decisions
| Decision | Rationale |
|---|---|
| Keep planning artifacts scoped under `.planning/next-stage-roadmap/` | Preserve completed planning records from the preceding implementation phase |
| Treat source/tests as authoritative and docs as supporting evidence | Avoid overstating partially implemented features based only on product copy |
| Do not edit product code | The user explicitly requested inventory and roadmap before implementation |

## Errors Encountered
| Error | Attempt | Resolution |
|---|---:|---|
| Combined broad `rg --files`/Git command yielded no captured output | 1 | Narrow scans by directory and use short, bounded commands |
| `list_mcp_resources(server="gitnexus")` could not start a named resource client | 1 | Discover available resource servers without a server filter; continue using the already available GitNexus query/context tools if resource discovery remains unavailable |
| GitNexus query tool is unavailable after its MCP server startup failure | 1 | Fall back to bounded `rg`, source/test inspection, protocol docs, and extension manifests; explicitly record that evidence method in the report |
| Direct reads of several unchanged source files intermittently blocked with no output | 1 | Interrupted the reader; use `git show HEAD:path` for unchanged modules and smaller bounded reads instead of repeating the same filesystem access |
