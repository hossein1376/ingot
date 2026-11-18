//! Every vector of `sha256_vectors.json` (P4a): the NIST strings, the million `a`s,
//! the `i % 251` patterns around block boundaries, and the package-tree framing of
//! 05 §33.2.

use ingot_diag::json::Json;
use ingot_driver::sha256::{digest, hash_tree, hex};
use std::path::PathBuf;

fn vectors() -> Json {
    let text = include_str!("data/sha256_vectors.json");
    ingot_diag::json::parse(text).expect("the vectors parse")
}

fn string(value: &Json, key: &str) -> String {
    value
        .get(key)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("`{key}` is a string"))
        .to_string()
}

#[test]
fn every_case_in_the_vector_file() {
    let json = vectors();
    let cases = match json.get("cases") {
        Some(Json::Array(cases)) => cases.clone(),
        other => panic!("`cases` is an array: {other:?}"),
    };
    assert!(!cases.is_empty());
    for case in &cases {
        let expected = string(case, "sha256");
        if let Some(text) = case.get("ascii").and_then(Json::as_str) {
            assert_eq!(hex(&digest(text.as_bytes())), expected, "ascii {text:?}");
            continue;
        }
        if let Some(text) = case.get("ascii_repeat").and_then(Json::as_str) {
            let count = case.get("count").and_then(Json::as_i64).expect("a count") as usize;
            assert_eq!(
                hex(&digest(text.repeat(count).as_bytes())),
                expected,
                "ascii_repeat {text:?} x {count}"
            );
            continue;
        }
        if let Some(len) = case.get("pattern_len").and_then(Json::as_i64) {
            let bytes: Vec<u8> = (0..len as usize).map(|i| (i % 251) as u8).collect();
            assert_eq!(hex(&digest(&bytes)), expected, "pattern_len {len}");
            continue;
        }
        panic!("a case with no kind: {case:?}");
    }
}

#[test]
fn the_package_tree_hashes_with_the_specified_framing() {
    let json = vectors();
    let tree = json.get("package_tree").expect("package_tree");
    let expected = string(tree, "hash");
    let files = match tree.get("files") {
        Some(Json::Object(files)) => files.clone(),
        other => panic!("`files` is an object: {other:?}"),
    };
    let dir = std::env::temp_dir().join(format!(
        "ingot-sha256-package-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (path, content) in &files {
        let path = PathBuf::from(path);
        let absolute = dir.join(&path);
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(absolute, content.as_str().unwrap()).unwrap();
    }
    let actual = hash_tree(&dir, &[]).expect("the tree hashes");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(actual, expected);
}
