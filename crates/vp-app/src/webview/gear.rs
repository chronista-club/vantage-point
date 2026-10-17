//! Verified Midistage gear assets. No caller-supplied filesystem paths.
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

const MAX_ASSET: u64 = 32 * 1024 * 1024;
const MAX_MANIFEST: u64 = 1024 * 1024;

fn bounded_read(root: &Path, filename: &str, limit: u64) -> Option<Vec<u8>> {
    let root = root.canonicalize().ok()?;
    let path = root.join(filename).canonicalize().ok()?;
    if !path.starts_with(&root) {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() || file.metadata().ok()?.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

/// Only a flat id + allowed extension (optionally pinned to a SHA-256) is accepted.
pub fn read_asset(root: &Path, request: &str) -> Option<(Vec<u8>, &'static str)> {
    let fields: Vec<_> = request.split('.').collect();
    let (id, expected, extension) = match fields.as_slice() {
        [id, ext] => (*id, None, *ext),
        [id, hash, ext] => (*id, Some(*hash), *ext),
        _ => return None,
    };
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return None;
    }
    let content_type = match extension {
        "glb" => "model/gltf-binary",
        "json" => "application/json",
        _ => return None,
    };
    let manifest_bytes = bounded_read(root, "manifest.json", MAX_MANIFEST)?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).ok()?;
    if manifest["schema"].as_u64()? != 1 || !manifest["gear"].is_object() {
        return None;
    }
    if request == "manifest.json" {
        return Some((manifest_bytes, content_type));
    }
    let descriptor = &manifest["gear"][id][extension];
    let hash = descriptor["sha256"].as_str()?;
    if hash.len() != 64
        || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        || expected.is_some_and(|value| value != hash)
    {
        return None;
    }
    let size = descriptor["bytes"].as_u64()?;
    if size == 0 || size > MAX_ASSET {
        return None;
    }
    let bytes = bounded_read(root, &format!("{id}.{extension}"), size)?;
    if bytes.len() as u64 != size
        || Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            != hash.to_ascii_lowercase()
    {
        return None;
    }
    Some((bytes, content_type))
}

pub fn gear_root() -> Option<std::path::PathBuf> {
    Some(dirs::data_dir()?.join("Midistage").join("gear"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("minilab.glb"), b"abc").unwrap();
        std::fs::write(dir.path().join("manifest.json"), serde_json::to_vec(&json!({
            "schema":1,"gear":{"minilab":{"glb":{"bytes":3,"sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}}}
        })).unwrap()).unwrap();
        dir
    }
    #[test]
    fn verified_asset_only() {
        let dir = fixture();
        assert_eq!(read_asset(dir.path(), "minilab.glb").unwrap().0, b"abc");
        let pinned = "minilab.ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad.glb";
        assert_eq!(read_asset(dir.path(), pinned).unwrap().0, b"abc");
        assert!(read_asset(dir.path(), &format!("minilab.{}.glb", "0".repeat(64))).is_none());
        assert!(read_asset(dir.path(), "maru.glb").is_none());
        std::fs::write(dir.path().join("minilab.glb"), b"bad").unwrap();
        assert!(read_asset(dir.path(), "minilab.glb").is_none());
    }
    #[test]
    fn rejects_unknown_schema_and_unsafe_paths() {
        let dir = fixture();
        for path in [
            "../minilab.glb",
            "/minilab.glb",
            "%2e%2e/minilab.glb",
            "minilab.blend",
            "minilab.glb?q=1",
            "x\\minilab.glb",
        ] {
            assert!(read_asset(dir.path(), path).is_none(), "{path}");
        }
        let path = dir.path().join("manifest.json");
        let data = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"schema\":1", "\"schema\":2");
        std::fs::write(path, data).unwrap();
        assert!(read_asset(dir.path(), "minilab.glb").is_none());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_outside_gear_including_manifest() {
        let dir = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("asset"), b"abc").unwrap();
        std::fs::remove_file(dir.path().join("minilab.glb")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("asset"), dir.path().join("minilab.glb"))
            .unwrap();
        assert!(read_asset(dir.path(), "minilab.glb").is_none());
        std::fs::rename(
            dir.path().join("manifest.json"),
            outside.path().join("manifest"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("manifest"),
            dir.path().join("manifest.json"),
        )
        .unwrap();
        assert!(read_asset(dir.path(), "manifest.json").is_none());
    }
}
