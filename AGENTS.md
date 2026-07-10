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
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release-check.ps1
```

The preflight checks version sync, PowerShell/WiX packaging files, formatting,
clippy, normal tests, and ignored performance baselines. Do not skip performance
baselines for release readiness.

Release packaging should be reproducible and offline. Preload Mesa in
`target/mesa-runtime/x64` with `mesa-runtime-manifest.json` or provide
`MESA_RUNTIME_DIR`/`third_party/mesa`. Preload ANGLE through `ANGLE_RUNTIME_DIR`,
`third_party/angle`, or `target/angle-runtime`; use `SCOPE_ALLOW_SYSTEM_ANGLE=1`
only for local experiments, not releases.

After preflight passes, build the release package with offline dependency
resolution:

```powershell
$env:SCOPE_PACKAGE_OFFLINE=1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-windows.ps1 -OfflinePackage
```
