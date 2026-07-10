use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=resources/ScopeAnalyzer.rc");
    println!("cargo:rerun-if-changed=resources/ScopeAnalyzer.ico");
    println!("cargo:rerun-if-changed=resources/ScopeAnalyzer.res");
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=LLVM_RC");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = match env::var_os("OUT_DIR") {
        Some(out_dir) => PathBuf::from(out_dir),
        None => return,
    };
    let output = out_dir.join("scope_analyzer_icon.res");

    if compile_resource(&output) || link_resource(Path::new("resources/ScopeAnalyzer.res")) {
        return;
    }

    println!(
        "cargo:warning=no Windows resource compiler found and resources/ScopeAnalyzer.res is missing; executable icon was not embedded"
    );
}

fn compile_resource(output: &Path) -> bool {
    let mut candidates = Vec::<(PathBuf, ResourceCompiler)>::new();
    if let Some(path) = env::var_os("WINDRES") {
        candidates.push((PathBuf::from(path), ResourceCompiler::Windres));
    }
    if let Some(path) = env::var_os("LLVM_RC") {
        candidates.push((PathBuf::from(path), ResourceCompiler::Rc));
    }
    if let Some(path) = env::var_os("RC") {
        candidates.push((PathBuf::from(path), ResourceCompiler::Rc));
    }

    candidates.extend([
        (PathBuf::from("llvm-rc"), ResourceCompiler::Rc),
        (PathBuf::from("rc"), ResourceCompiler::Rc),
        (PathBuf::from(default_windres()), ResourceCompiler::Windres),
        (PathBuf::from("windres"), ResourceCompiler::Windres),
    ]);

    for (program, compiler) in candidates {
        if run_resource_compiler(&program, compiler, output) {
            link_resource(output);
            return true;
        }
    }
    false
}

fn default_windres() -> &'static str {
    match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x86_64-w64-mingw32-windres",
        _ => "windres",
    }
}

#[derive(Clone, Copy)]
enum ResourceCompiler {
    Rc,
    Windres,
}

fn run_resource_compiler(program: &Path, compiler: ResourceCompiler, output: &Path) -> bool {
    let output_arg = output.to_string_lossy().into_owned();
    let status = match compiler {
        ResourceCompiler::Rc => Command::new(program)
            .args([
                "/nologo",
                "/fo",
                output_arg.as_str(),
                "resources/ScopeAnalyzer.rc",
            ])
            .status(),
        ResourceCompiler::Windres => Command::new(program)
            .args([
                "resources/ScopeAnalyzer.rc",
                "-O",
                "coff",
                "-o",
                output_arg.as_str(),
            ])
            .status(),
    };
    matches!(status, Ok(status) if status.success())
}

fn link_resource(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let path = path.to_string_lossy().replace('\\', "/");
    println!("cargo:rustc-link-arg-bins={path}");
    true
}
