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
