# Repository Rules

## Versioning

Major changes must update the application version in the same change set.

A change counts as major when it changes user-facing behavior, data import/export
formats, packaged installer behavior, keyboard or layout defaults, public
configuration files, or introduces a breaking workflow change.

When bumping the version, keep these files in sync:

- `Cargo.toml` package `version`
- `scripts/package-windows.ps1` `$version`
- `scripts/ScopeAnalyzer.wxs` `Product Version`
- README package artifact names when they include the version

Small bug fixes, internal refactors, tests, and documentation-only changes do not
require a version bump unless they are released as a new installer build.

## Release Procedure

Before producing release artifacts, run the one-command preflight from the
repository root:

```powershell
$env:SCOPE_PACKAGE_OFFLINE=1
$env:CARGO_NET_OFFLINE=true
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release-check.ps1
```

The preflight checks version sync, PowerShell/WiX packaging files, formatting,
clippy, normal tests, and ignored performance baselines. Do not skip performance
baselines for release readiness.

Release packaging should be reproducible and offline. Preload Rust 1.96.0 and
the Cargo registry/git/target caches on the controlled release runner. The
controlled Mesa and ANGLE runtimes must live outside the checkout because
`actions/checkout` cleans ignored workspace paths. Set `MESA_RUNTIME_DIR` to a
directory containing the pinned `mesa-runtime-manifest.json`, and set
`ANGLE_RUNTIME_DIR` to a directory containing a hash-pinned
`angle-runtime-preload-manifest.json`. Release packaging must set both
`ANGLE_RUNTIME_SOURCE_SHA256` (the source asset hash) and
`ANGLE_RUNTIME_MANIFEST_SHA256` (the preload manifest hash); the manifest binds
that source hash to every copied ANGLE DLL. `target/mesa-runtime/x64` and
`target/angle-runtime` are local-development caches only. Use
`SCOPE_ALLOW_SYSTEM_ANGLE=1` only for local experiments, not releases.

After preflight passes, build the release package with offline dependency
resolution:

```powershell
$env:SCOPE_PACKAGE_OFFLINE=1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-windows.ps1 -OfflinePackage
```

The controlled release workflow also sets `CARGO_NET_OFFLINE=true` so the
preflight cannot silently resolve missing dependencies from the network.

Hardware acceptance uses the dedicated `scope-hardware-smoke` binary on a
runner physically connected to an SCP1 device; the simulator test is not
hardware evidence. Archive its `.scope` output, JSON envelope, and the
`scope-cli validate-recording` result.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **Scope** (4572 symbols, 18036 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "main"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({search_query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method without first running `impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit changes without running `detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/Scope/context` | Codebase overview, check index freshness |
| `gitnexus://repo/Scope/clusters` | All functional areas |
| `gitnexus://repo/Scope/processes` | All execution flows |
| `gitnexus://repo/Scope/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
