//! 同じ state directory の GUI application を同時に起動しない。
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub(super) fn acquire(state_dir: &Path, _index: usize) -> io::Result<Option<File>> {
    let dir = state_dir.join("app-instances");
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("0.lock"))?;
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

    // mem_1Cfd9HUeYqn4iKm5YXvgsj: window numbers must not create extra applications.
    #[test]
    fn different_window_numbers_share_one_application_lock() {
        let dir = tempfile::tempdir().unwrap();
        let owner = acquire(dir.path(), 0).unwrap().unwrap();
        assert!(acquire(dir.path(), 1).unwrap().is_none());
        assert!(acquire(dir.path(), 2).unwrap().is_none());
        drop(owner);
        assert!(acquire(dir.path(), 2).unwrap().is_some());
    }

    #[test]
    fn window_instance_excludes_duplicate_and_releases_after_owner_exit() {
        let dir = tempfile::tempdir().unwrap();
        let owner = acquire(dir.path(), 1).unwrap().unwrap();
        assert!(acquire(dir.path(), 1).unwrap().is_none());
        assert!(acquire(dir.path(), 2).unwrap().is_none());
        drop(owner);
        assert!(acquire(dir.path(), 1).unwrap().is_some());
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
