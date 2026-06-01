use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=resources/ScopeAnalyzer.rc");
    println!("cargo:rerun-if-changed=resources/ScopeAnalyzer.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = match env::var_os("OUT_DIR") {
        Some(out_dir) => PathBuf::from(out_dir),
        None => return,
    };
    let output = out_dir.join("scope_analyzer_icon.res");
    let output_arg = output.to_string_lossy().into_owned();
    let windres = env::var_os("WINDRES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("windres"));

    match Command::new(&windres)
        .args([
            "resources/ScopeAnalyzer.rc",
            "-O",
            "coff",
            "-o",
            output_arg.as_str(),
        ])
        .status()
    {
        Ok(status) if status.success() => {
            println!("cargo:rustc-link-arg-bins={}", output.display());
        }
        Ok(status) => {
            println!(
                "cargo:warning=windres exited with status {status}; executable icon was not embedded"
            );
        }
        Err(error) => {
            println!(
                "cargo:warning=failed to run {}: {error}; executable icon was not embedded",
                windres.display()
            );
        }
    }
}
