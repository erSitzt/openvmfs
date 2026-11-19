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

/// The example in `examples/export_disk.c` is what the C guide shows; it must keep compiling and working.
#[test]
fn the_c_example_exports_a_disk() {
    if cfg!(not(target_os = "linux")) {
        eprintln!("skipping: the C example is built on Linux only");
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
    let program = profile_dir.join("openvmfs-c-export");
    let compile = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-o"])
        .arg(&program)
        .arg(manifest.join("examples/export_disk.c"))
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
        "compiling export_disk.c failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // `normal.vmdk` is a thin disk of 69 GiB with one MiB of data at 42 GiB.
    let output = profile_dir.join(format!("openvmfs-c-export-{}.raw", std::process::id()));
    let run = Command::new(&program)
        .arg(&image)
        .arg("normal.vmdk")
        .arg(&output)
        .env("LD_LIBRARY_PATH", &profile_dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    // Portable reads: this file is compiled on Windows too, although the test only runs on Linux.
    use std::io::{Read, Seek, SeekFrom};
    let mut exported = std::fs::File::open(&output).unwrap();
    assert_eq!(exported.metadata().unwrap().len(), 69 << 30);
    let mut at_data = [0u8; 4096];
    exported.seek(SeekFrom::Start(42 << 30)).unwrap();
    exported.read_exact(&mut at_data).unwrap();
    assert!(at_data.iter().any(|&byte| byte != 0));
    let mut before = [0xFFu8; 4096];
    exported.seek(SeekFrom::Start((42 << 30) - 4096)).unwrap();
    exported.read_exact(&mut before).unwrap();
    assert!(before.iter().all(|&byte| byte == 0));
    drop(exported);
    std::fs::remove_file(&output).unwrap();

    // A missing disk is reported through the error object and a non-zero exit.
    let missing = Command::new(&program)
        .arg(&image)
        .arg("no-such-disk.vmdk")
        .arg(&output)
        .env("LD_LIBRARY_PATH", &profile_dir)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("status 4"));
}
