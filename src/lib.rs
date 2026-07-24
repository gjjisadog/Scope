pub mod compare;
pub mod data;
pub mod live;
pub mod measurements;
pub mod plot_viewport;
pub mod presentation;
pub mod project;
pub mod rules;

#[cfg(test)]
mod release_tests {
    #[test]
    fn live_scope_release_version_is_synchronized() {
        const VERSION: &str = "0.15.0";
        let cargo_toml = include_str!("../Cargo.toml");
        let package_script = include_str!("../scripts/package-windows.ps1");
        let wix = include_str!("../scripts/ScopeAnalyzer.wxs");
        let readme = include_str!("../README.md");

        assert_eq!(env!("CARGO_PKG_VERSION"), VERSION);
        assert!(cargo_toml.contains(&format!("version = \"{VERSION}\"")));
        assert!(package_script.contains(&format!("$version = \"{VERSION}\"")));
        assert!(wix.contains(&format!("Version=\"{VERSION}\"")));
        assert!(readme.contains(&format!("ScopeAnalyzer-{VERSION}-win-x64.zip")));
        assert!(readme.contains(&format!("ScopeAnalyzer-{VERSION}-win-x64.msi")));
    }
}
