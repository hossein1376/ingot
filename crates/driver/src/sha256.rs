//! SHA-256 (FIPS 180-4), written here because the compiler takes no crates.
//!
//! [`hash_tree`] is the dependency hash of 05 §33.2: every `ingot.toml` and every
//! `.ingot` file in the tree (nested packages included, a `.git` directory excluded),
//! plus every file a manifest names, framed and hashed. The result is written as
//! `sha256:` and the 64 lowercase hex digits.

use std::path::{Path, PathBuf};

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// The 32-byte SHA-256 digest of `bytes`.
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut state = INIT;
    let mut offset = 0;
    while offset + 64 <= bytes.len() {
        let mut block = [0u8; 64];
        block.copy_from_slice(&bytes[offset..offset + 64]);
        compress(&mut state, &block);
        offset += 64;
    }
    let rest = &bytes[offset..];
    let mut tail = Vec::with_capacity(128);
    tail.extend_from_slice(rest);
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&((bytes.len() as u64).wrapping_mul(8)).to_be_bytes());
    let mut offset = 0;
    while offset + 64 <= tail.len() {
        let mut block = [0u8; 64];
        block.copy_from_slice(&tail[offset..offset + 64]);
        compress(&mut state, &block);
        offset += 64;
    }
    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

/// The lowercase hex spelling of a digest.
pub fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// One file of a dependency tree: its relative path with `/` separators and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FramedFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// The framed hash of `files` in byte order of their paths (05 §33.2). Duplicate
/// paths keep the first.
pub fn hash_files(files: &[FramedFile]) -> String {
    let mut files: Vec<&FramedFile> = files.iter().collect();
    files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    files.dedup_by(|a, b| a.path == b.path);
    let mut bytes = Vec::new();
    for file in files {
        bytes.extend_from_slice(&(file.path.len() as u64).to_le_bytes());
        bytes.extend_from_slice(file.path.as_bytes());
        bytes.extend_from_slice(&(file.bytes.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&file.bytes);
    }
    format!("sha256:{}", hex(&digest(&bytes)))
}

/// Every `ingot.toml` and every `.ingot` file below `root`, plus `extra` files
/// (assembly sources a manifest names), as the dependency hash frames them. A `.git`
/// directory is excluded. The returned files are sorted by path.
pub fn tree_files(root: &Path, extra: &[(String, PathBuf)]) -> std::io::Result<Vec<FramedFile>> {
    let mut out = Vec::new();
    collect(root, root, &mut out)?;
    for (rel, absolute) in extra {
        if out.iter().any(|f| &f.path == rel) {
            continue;
        }
        let bytes = match std::fs::read(absolute) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("`{rel}` is named by a manifest but does not exist"),
                ));
            }
            Err(error) => return Err(error),
        };
        out.push(FramedFile {
            path: rel.clone(),
            bytes,
        });
    }
    out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(out)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<FramedFile>) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        // Follow symlinks exactly as the loader's `Files` reads them: the hash must
        // cover the bytes a build can read, not the link text.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if name == ".git" {
                continue;
            }
            collect(root, &path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "ingot") || name == "ingot.toml" {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push(FramedFile {
                path: rel,
                bytes: std::fs::read(&path)?,
            });
        }
    }
    Ok(())
}

/// `tree_files` and `hash_files` for a directory.
pub fn hash_tree(root: &Path, extra: &[(String, PathBuf)]) -> std::io::Result<String> {
    Ok(hash_files(&tree_files(root, extra)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_of(bytes: &[u8]) -> String {
        hex(&digest(bytes))
    }

    #[test]
    fn the_nist_strings() {
        assert_eq!(
            hex_of(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_of(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex_of(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn the_million_as() {
        let bytes = vec![b'a'; 1_000_000];
        assert_eq!(
            hex_of(&bytes),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn the_pattern_around_block_boundaries() {
        let cases = [
            (
                1,
                "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
            ),
            (
                55,
                "463eb28e72f82e0a96c0a4cc53690c571281131f672aa229e0d45ae59b598b59",
            ),
            (
                56,
                "da2ae4d6b36748f2a318f23e7ab1dfdf45acdc9d049bd80e59de82a60895f562",
            ),
            (
                57,
                "2fe741af801cc238602ac0ec6a7b0c3a8a87c7fc7d7f02a3fe03d1c12eac4d8f",
            ),
            (
                63,
                "29af2686fd53374a36b0846694cc342177e428d1647515f078784d69cdb9e488",
            ),
            (
                64,
                "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108",
            ),
            (
                65,
                "4bfd2c8b6f1eec7a2afeb48b934ee4b2694182027e6d0fc075074f2fabb31781",
            ),
            (
                119,
                "da18797ed7c3a777f0847f429724a2d8cd5138e6ed2895c3fa1a6d39d18f7ec6",
            ),
            (
                120,
                "f52b23db1fbb6ded89ef42a23ce0c8922c45f25c50b568a93bf1c075420bbb7c",
            ),
            (
                127,
                "92ca0fa6651ee2f97b884b7246a562fa71250fedefe5ebf270d31c546bfea976",
            ),
            (
                128,
                "471fb943aa23c511f6f72f8d1652d9c880cfa392ad80503120547703e56a2be5",
            ),
            (
                129,
                "5099c6a56203f9687f7d33f4bfdf576d31dc91f6b695ecea38b2770c87631135",
            ),
            (
                1000,
                "4e4c294b331f7a2099a379bec34b9f9fc03dc46ab465d998f4d683da53487e6d",
            ),
            (
                4096,
                "d67c656e01756650d77717b0839985a056ec28ffe174601d690fc407a2ceffca",
            ),
            (
                65537,
                "237356e18b503616912abb8ffaed3a72591e397d4ac294c4637917d48a3f529d",
            ),
        ];
        for (len, expected) in cases {
            let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(hex_of(&bytes), expected, "pattern_len {len}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_hashed_as_the_loader_reads_it() {
        let dir = std::env::temp_dir().join(format!(
            "ingot-sha256-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.ingot"), b"pub fn one() { }\n").unwrap();
        std::os::unix::fs::symlink("main.ingot", dir.join("link.ingot")).unwrap();
        let actual = hash_tree(&dir, &[]).unwrap();
        let expected = hash_files(&[
            FramedFile {
                path: "link.ingot".to_string(),
                bytes: b"pub fn one() { }\n".to_vec(),
            },
            FramedFile {
                path: "main.ingot".to_string(),
                bytes: b"pub fn one() { }\n".to_vec(),
            },
        ]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(actual, expected);
    }

    #[test]
    fn the_package_tree_framing() {
        let files = vec![
            FramedFile {
                path: "Z.ingot".to_string(),
                bytes: Vec::new(),
            },
            FramedFile {
                path: "ingot.toml".to_string(),
                bytes: b"name = \"lib\"\n".to_vec(),
            },
            FramedFile {
                path: "main.ingot".to_string(),
                bytes: b"pub fn one() -> i64 { return 1; }\n".to_vec(),
            },
            FramedFile {
                path: "util/x.ingot".to_string(),
                bytes: b"pub fn two() -> i64 { return 2; }\n".to_vec(),
            },
        ];
        assert_eq!(
            hash_files(&files),
            "sha256:b1caafbb93ca95cd83d2bea3655e56bed8626b2ff3e1a76155cbdddca5cbce72"
        );
    }
}
