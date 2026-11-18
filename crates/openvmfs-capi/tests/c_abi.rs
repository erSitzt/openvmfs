//! Compiles a C program against `include/openvmfs.h`, links it with the shared library and runs it on a sample
//! image. This checks that the header and the library agree, from the caller's side.
//!
//! Skips when no C compiler or no sample image is available.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Finds the shared library that cargo built for this test run. Cargo's directory layout differs between versions
/// (`target/<profile>/deps` or `target/<profile>/build/<package>/<hash>/out`), so the newest match under the
/// profile directory is taken.
fn find_library() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe
        .ancestors()
        .find(|dir| dir.parent().is_some_and(|parent| parent.ends_with("target")))?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut pending = vec![(profile_dir.to_path_buf(), 0)];
    while let Some((dir, depth)) = pending.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 4 && !is_noise(&path) {
                    pending.push((path, depth + 1));
                }
            } else if path.file_name().is_some_and(|name| name == "libopenvmfs_c.so") {
                let modified = entry.metadata().ok()?.modified().ok()?;
                if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
                    newest = Some((modified, path));
                }
            }
        }
    }
    newest.map(|(_, path)| path)
}

fn is_noise(dir: &Path) -> bool {
    dir.file_name()
        .is_some_and(|name| name == "incremental" || name == "fixture-cache" || name == ".fingerprint")
}

#[test]
fn a_c_program_can_use_the_library() {
    if cfg!(not(target_os = "linux")) {
        eprintln!("skipping: the C smoke test is wired up for Linux only");
        return;
    }
    let Some(image) = openvmfs_testkit::external_image("vmfs/vmfs6") else {
        return;
    };
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(library) = find_library() else {
        panic!("libopenvmfs_c.so was not found next to the test binary");
    };
    let profile_dir = library.parent().map(PathBuf::from).unwrap_or_default();

    let program = profile_dir.join("openvmfs-c-smoke");
    let compile = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-o"])
        .arg(&program)
        .arg(manifest.join("tests/smoke.c"))
        .arg("-I")
        .arg(manifest.join("include"))
        .arg("-L")
        .arg(&profile_dir)
        .arg("-lopenvmfs_c")
        .output();
    let compile = match compile {
        Ok(output) => output,
        Err(error) => {
            eprintln!("skipping: no C compiler ({error})");
            return;
        }
    };
    assert!(
        compile.status.success(),
        "compiling smoke.c failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&program)
        .arg(&image)
        .env("LD_LIBRARY_PATH", &profile_dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "smoke program failed: {}\n{}",
        stdout,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(stdout.starts_with("OK "), "unexpected output: {stdout}");
}
