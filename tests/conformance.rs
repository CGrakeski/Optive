#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

fn fixtures() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance");
    let mut files: Vec<_> = fs::read_dir(root)
        .expect("conformance/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "tive"))
        .collect();
    files.sort();
    files
}

#[test]
fn conformance_fixtures_run() {
    let files = fixtures();
    assert!(!files.is_empty(), "expected versioned conformance fixtures");
    for path in files {
        let source = fs::read_to_string(&path).expect("read fixture");
        optive::run_source(&source).unwrap_or_else(|e| {
            panic!("{} failed: {e}", path.display());
        });
    }
}
