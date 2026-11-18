//! Unbuffered reading (`O_DIRECT`): the same bytes as buffered reading, for any offset and length.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use std::io::Write;

use openvmfs::{BlockSource, ErrorKind, FileSource, OpenOptions, Volume};
use openvmfs_testkit::{cache_dir, own_image, sha256_of, Rng};

/// Opens `path` unbuffered, or returns `None` where the filesystem refuses that (tmpfs does).
fn open_direct(path: &std::path::Path) -> Option<FileSource> {
    match FileSource::open_direct(path) {
        Ok(source) => Some(source),
        Err(error) if matches!(error.kind(), ErrorKind::Io | ErrorKind::Unsupported) => {
            eprintln!("skipping: {error}");
            None
        }
        Err(error) => panic!("{error}"),
    }
}

#[test]
fn any_offset_and_length_reads_the_same_as_buffered() {
    // A file whose size is not a multiple of the block size, so that the last block comes back short.
    let dir = cache_dir().join("tmp");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("direct-io-{}.bin", std::process::id()));
    let mut rng = Rng::new(7);
    let content: Vec<u8> = (0..(3 << 20) + 1234).map(|_| rng.next_u64() as u8).collect();
    std::fs::File::create(&path).unwrap().write_all(&content).unwrap();

    if let Some(direct) = open_direct(&path) {
        assert_eq!(direct.len(), content.len() as u64);
        let len = content.len() as u64;
        let mut cases = vec![
            (0u64, 1usize),
            (0, 4096),
            (1, 4095),
            (4095, 2),
            (4096, 4096),
            (len - 1, 1),
            (len - 5000, 5000),
            (0, content.len()),
        ];
        for _ in 0..200 {
            let offset = rng.below(len);
            cases.push((offset, rng.below((len - offset).min(300_000)) as usize));
        }
        for (offset, length) in cases {
            let mut buf = vec![0xEEu8; length];
            direct.read_exact_at(offset, &mut buf).unwrap();
            assert_eq!(
                buf,
                content[offset as usize..offset as usize + length],
                "offset {offset} length {length}"
            );
        }
        let mut beyond = [0u8; 2];
        assert!(direct.read_exact_at(len - 1, &mut beyond).is_err());
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_volume_reads_the_same_unbuffered() {
    let image = own_image("fixture-zero-v1");
    let options = OpenOptions {
        direct: true,
        ..OpenOptions::default()
    };
    let direct = match Volume::open_paths(&[&image], &options) {
        Ok(volume) => volume,
        Err(error) if error.kind() == ErrorKind::Io => {
            eprintln!("skipping: {error}");
            return;
        }
        Err(error) => panic!("{error}"),
    };
    let buffered = Volume::open_path(&image).unwrap();
    assert_eq!(direct.info(), buffered.info());
    let mut compared = 0;
    for entry in buffered.read_dir("pat").unwrap() {
        if entry.name.ends_with(".vmdk") && !entry.name.contains("-flat") {
            let path = format!("pat/{}", entry.name);
            let (Ok(a), Ok(b)) = (buffered.open_disk(&path), direct.open_disk(&path)) else {
                continue;
            };
            assert_eq!(sha256_of(&a), sha256_of(&b), "{path}");
            compared += 1;
        }
    }
    assert!(compared > 0);
}
