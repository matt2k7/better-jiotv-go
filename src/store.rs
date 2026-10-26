//! A tiny TOML key/value store, file-compatible with the Go version's
//! `store_v4.toml` (`{ data = { k = "v", ... } }`), so an existing store
//! keeps working when a router is moved from the Go binary to this one.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    #[serde(default)]
    data: BTreeMap<String, String>,
}

pub struct Store {
    path: PathBuf,
    inner: Mutex<FileFormat>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Returned by `get` (the non-`Option` companion to `get_opt`, which
    /// every current caller uses instead); kept as public API.
    #[allow(dead_code)]
    #[error("key not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Decode(#[from] toml::de::Error),
    #[error(transparent)]
    Encode(#[from] toml::ser::Error),
}

impl Store {
    pub fn open(path_prefix: &str) -> Result<Store, StoreError> {
        std::fs::create_dir_all(path_prefix)?;
        let path = PathBuf::from(path_prefix).join("store_v4.toml");
        let inner = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str(&text)?
        } else {
            let f = FileFormat::default();
            let store = Store {
                path: path.clone(),
                inner: Mutex::new(f),
            };
            store.save()?;
            return Ok(store);
        };
        Ok(Store {
            path,
            inner: Mutex::new(inner),
        })
    }

    fn save(&self) -> Result<(), StoreError> {
        let guard = self.inner.lock().unwrap();
        self.write_atomic(&guard)
    }

    /// Writes the whole file via a temp file in the same directory and a
    /// rename, so a power cut leaves either the old or the new file, never a
    /// truncated one. Callers hold the lock, which keeps writes ordered.
    fn write_atomic(&self, data: &FileFormat) -> Result<(), StoreError> {
        let text = toml::to_string(data)?;
        let tmp = self.path.with_extension("toml.tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
        }
        if let Ok(meta) = std::fs::metadata(&self.path) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Sets and removes several keys with at most one write. The file is not
    /// touched at all if nothing actually changes, which spares flash/SD cards
    /// the redundant rewrites a key-by-key save would make.
    pub fn apply(&self, set: &[(&str, &str)], remove: &[&str]) -> Result<(), StoreError> {
        let mut guard = self.inner.lock().unwrap();
        let mut changed = false;
        for (key, value) in set {
            if guard.data.get(*key).map(String::as_str) != Some(*value) {
                guard.data.insert((*key).to_string(), (*value).to_string());
                changed = true;
            }
        }
        for key in remove {
            changed |= guard.data.remove(*key).is_some();
        }
        if changed {
            self.write_atomic(&guard)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Result<String, StoreError> {
        let guard = self.inner.lock().unwrap();
        guard
            .data
            .get(key)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(key.to_string()))
    }

    pub fn get_opt(&self, key: &str) -> Option<String> {
        self.inner.lock().unwrap().data.get(key).cloned()
    }

    pub fn set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.apply(&[(key, value)], &[])
    }

    pub fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.apply(&[], &[key])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_values() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_str().unwrap()).unwrap();
        store.set("access_key", "abc123").unwrap();
        assert_eq!(store.get("access_key").unwrap(), "abc123");
        store.delete("access_key").unwrap();
        assert!(store.get("access_key").is_err());
    }

    #[test]
    fn reads_go_format_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store_v4.toml");
        std::fs::write(&path, "[data]\naccess_key = \"deadbeef\"\n").unwrap();
        let store = Store::open(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(store.get("access_key").unwrap(), "deadbeef");
    }

    #[test]
    fn apply_batches_and_skips_unchanged_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().to_str().unwrap()).unwrap();
        store
            .apply(&[("a", "1"), ("b", "2")], &["missing"])
            .unwrap();
        let file = dir.path().join("store_v4.toml");
        // Remove the file: an unchanged apply must not recreate it...
        std::fs::remove_file(&file).unwrap();
        store.apply(&[("a", "1"), ("b", "2")], &["missing"]).unwrap();
        store.set("a", "1").unwrap();
        store.delete("missing").unwrap();
        assert!(!file.exists());
        // ...while a real change writes it again, and leaves no temp file.
        store.apply(&[("a", "9")], &["b"]).unwrap();
        assert!(file.exists());
        assert!(!dir.path().join("store_v4.toml.tmp").exists());
        let reopened = Store::open(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(reopened.get("a").unwrap(), "9");
        assert!(reopened.get("b").is_err());
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path().to_str().unwrap()).unwrap();
            store.set("k", "v").unwrap();
        }
        let store = Store::open(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(store.get("k").unwrap(), "v");
    }
}
