//! 同じ state directory / window 番号の GUI を同時に起動しない。
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub(super) fn acquire(state_dir: &Path, index: usize) -> io::Result<Option<File>> {
    let dir = state_dir.join("app-instances");
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{index}.lock")))?;
    // lock file は unlink しない。同じ inode に対する OS lock を使い、異常終了でも解放される。
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_instance_excludes_duplicate_and_releases_after_owner_exit() {
        let dir = tempfile::tempdir().unwrap();
        let owner = acquire(dir.path(), 1).unwrap().unwrap();
        assert!(acquire(dir.path(), 1).unwrap().is_none());
        let other = acquire(dir.path(), 2).unwrap().unwrap();
        drop(owner);
        assert!(acquire(dir.path(), 1).unwrap().is_some());
        assert!(acquire(dir.path(), 2).unwrap().is_none());
        drop(other);
    }

    #[test]
    fn window_instance_is_isolated_between_profiles() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let _a = acquire(first.path(), 0).unwrap().unwrap();
        let _b = acquire(second.path(), 0).unwrap().unwrap();
        assert!(acquire(first.path(), 0).unwrap().is_none());
    }
}
