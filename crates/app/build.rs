use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=resources/windows/r-shell.rc");
    println!("cargo:rerun-if-changed=resources/windows/r-shell.ico");

    if env::var_os("CARGO_FEATURE_GTK_UI").is_none() {
        return;
    }

    #[cfg(windows)]
    compile_windows_resources();
}

#[cfg(windows)]
fn compile_windows_resources() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let output = out_dir.join("r_shell_icon.o");
    let windres = find_windres().unwrap_or_else(|| {
        panic!("windres.exe was not found. Install MSYS2 mingw-w64-x86_64-gcc or set WINDRES.")
    });

    let status = Command::new(&windres)
        .current_dir(&manifest_dir)
        .args(["--input", "resources/windows/r-shell.rc", "--output"])
        .arg(&output)
        .args(["--output-format", "coff"])
        .status()
        .unwrap_or_else(|err| panic!("failed to run {}: {err}", windres.display()));

    if !status.success() {
        panic!("{} failed with status {status}", windres.display());
    }

    println!("cargo:rustc-link-arg-bins={}", output.display());
}

#[cfg(windows)]
fn find_windres() -> Option<PathBuf> {
    if let Some(path) = env::var_os("WINDRES") {
        return Some(PathBuf::from(path));
    }

    [
        "windres.exe",
        "x86_64-w64-mingw32-windres.exe",
        "windres",
        "x86_64-w64-mingw32-windres",
    ]
    .into_iter()
    .find_map(find_on_path)
}

#[cfg(windows)]
fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_file(candidate))
}

#[cfg(windows)]
fn is_file(path: &Path) -> bool {
    path.is_file()
}
