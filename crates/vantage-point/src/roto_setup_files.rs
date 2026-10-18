//! Shared state-zone input and confirmed-content cache. Never opens a device.
use crate::{
    roto_midi::VP_ROTO_SETUP,
    roto_setup::{Generated, SETUP, generate},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) struct Pins {
    path: PathBuf,
    last_bytes: Option<Vec<u8>>,
    last_good: Option<Generated>,
    read_error: bool,
}

impl Pins {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("midi-pins.json"),
            last_bytes: None,
            last_good: None,
            read_error: false,
        }
    }
    pub fn refresh(&mut self) -> Option<Generated> {
        let read = (|| -> std::io::Result<Vec<u8>> {
            let file = match std::fs::File::open(&self.path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(b"null".to_vec()),
                Err(e) => return Err(e),
            };
            let mut bytes = Vec::new();
            file.take(16_385).read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match read {
            Ok(bytes) => {
                self.read_error = false;
                if self.last_bytes.as_ref() != Some(&bytes) {
                    let parsed = serde_json::from_slice::<Value>(&bytes)
                        .ok()
                        .filter(|v| bytes.len() <= 16_384 && valid_pins(v));
                    if let Some(pins) = parsed {
                        self.last_good = Some(generate(&pins, &BTreeMap::new(), &VP_ROTO_SETUP));
                    } else {
                        tracing::warn!(
                            "ROTO setup: invalid midi-pins.json; keeping last valid pins"
                        );
                    }
                    self.last_bytes = Some(bytes);
                }
            }
            Err(error) => {
                if !self.read_error {
                    tracing::warn!(%error, "ROTO setup: cannot read pins");
                }
                self.read_error = true;
            }
        }
        self.last_good.clone()
    }
}

fn valid_pins(value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    let (Some(slots), Some(colors)) = (value["slots"].as_array(), value["colors"].as_array())
    else {
        return false;
    };
    matches!(slots.len(), 4 | 8)
        && slots.len() == colors.len()
        && slots.iter().all(|p| {
            p.is_null()
                || (p["address"].as_str().is_some_and(|s| !s.is_empty())
                    && p["path"].as_str().is_some_and(|s| !s.is_empty()))
        })
        && colors.iter().all(|c| {
            c.as_str().is_some_and(|s| {
                s.len() == 7
                    && s.starts_with('#')
                    && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            })
        })
}

pub(super) struct Hashes {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

impl Hashes {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("roto-setup-hashes.json");
        let values = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|v| v["version"] == 1 && v["setup"] == SETUP)
            .and_then(|v| serde_json::from_value(v["devices"].clone()).ok())
            .unwrap_or_default();
        Self { path, values }
    }
    pub fn get(&self, device: &str) -> Option<String> {
        self.values.get(device).cloned()
    }
    pub fn invalidate(&mut self, device: &str) -> anyhow::Result<()> {
        if let Some(previous) = self.values.remove(device)
            && let Err(error) = self.save()
        {
            self.values.insert(device.to_owned(), previous);
            return Err(error);
        }
        Ok(())
    }
    pub fn confirm(&mut self, device: &str, hash: &str) -> anyhow::Result<()> {
        self.values.insert(device.to_owned(), hash.to_owned());
        self.save()
    }
    fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(self.path.parent().expect("state file has parent"))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(&json!({"version":1, "setup":SETUP, "devices":self.values}))?,
        )?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_pin(dir: &Path, address: &str) {
        std::fs::write(dir.join("midi-pins.json.tmp"), json!({
            "slots":[{"path":"/vp", "address":address},null,null,null,null,null,null,null],
            "colors":["#ff0000", "#639af5", "#b78aee", "#f0b45a", "#c4d655", "#ef7b72", "#6fcf7e", "#e47ac9"],
            "dimGain":0.25,
        }).to_string()).unwrap();
        std::fs::rename(dir.join("midi-pins.json.tmp"), dir.join("midi-pins.json")).unwrap();
    }

    #[test]
    fn pins_reload_after_atomic_save_and_keep_last_valid_data_on_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = Pins::new(dir.path());
        let empty = source.refresh().unwrap();
        write_pin(dir.path(), "vp/first");
        let first = source.refresh().unwrap();
        assert_ne!(empty.hash, first.hash);
        std::fs::write(dir.path().join("midi-pins.json"), "broken").unwrap();
        assert_eq!(source.refresh().unwrap().hash, first.hash);
        assert_eq!(source.refresh().unwrap().hash, first.hash);
        std::fs::write(dir.path().join("midi-pins.json"), "{}").unwrap();
        assert_eq!(
            source.refresh().unwrap().hash,
            first.hash,
            "invalid object must not clear hardware pins"
        );
        write_pin(dir.path(), "vp/latest");
        assert_ne!(source.refresh().unwrap().hash, first.hash);
    }

    #[test]
    fn hashes_persist_only_explicit_confirmations_and_are_device_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let mut hashes = Hashes::load(dir.path());
        assert!(!dir.path().join("roto-setup-hashes.json").exists());
        hashes.confirm("roto-one", "a").unwrap();
        let saved = Hashes::load(dir.path());
        assert_eq!(saved.get("roto-one").as_deref(), Some("a"));
        assert_eq!(saved.get("roto-two"), None);
        std::fs::write(dir.path().join("roto-setup-hashes.json"), "broken").unwrap();
        assert_eq!(Hashes::load(dir.path()).get("roto-one"), None);
    }

    #[test]
    fn invalidation_survives_restart_without_touching_other_devices() {
        let dir = tempfile::tempdir().unwrap();
        let mut hashes = Hashes::load(dir.path());
        hashes.confirm("roto-one", "a").unwrap();
        hashes.confirm("roto-two", "b").unwrap();
        hashes.invalidate("roto-one").unwrap();
        let loaded = Hashes::load(dir.path());
        assert_eq!(loaded.get("roto-one"), None);
        assert_eq!(loaded.get("roto-two").as_deref(), Some("b"));
    }

    #[test]
    fn failed_invalidation_must_keep_blocking_writes_on_the_next_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let mut hashes = Hashes::load(dir.path());
        hashes.confirm("roto", "old").unwrap();
        std::fs::create_dir(dir.path().join("roto-setup-hashes.json.tmp")).unwrap();
        assert!(hashes.invalidate("roto").is_err());
        assert!(
            hashes.invalidate("roto").is_err(),
            "old persisted hash is still present"
        );
    }
}
