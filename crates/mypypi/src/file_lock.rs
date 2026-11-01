#![forbid(unsafe_code)]

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

#[derive(Debug)]
pub enum LockError {
    TimedOut,
    Io(std::io::Error),
}

impl FileLock {
    pub fn acquire(path: &Path, wait: Duration) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(LockError::Io)?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(TryLockError::WouldBlock) => return Err(LockError::TimedOut),
                Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_is_not_taken_over_when_its_file_names_a_dead_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mypypi.lock");
        let _holder = FileLock::acquire(&path, Duration::ZERO).unwrap();
        std::fs::write(&path, "999999\n").unwrap();
        let second = FileLock::acquire(&path, Duration::from_millis(250));
        assert!(matches!(second, Err(LockError::TimedOut)), "{second:?}");
    }

    #[test]
    fn dropping_the_holder_releases_the_lock_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mypypi.lock");
        drop(FileLock::acquire(&path, Duration::ZERO).unwrap());
        assert!(path.exists());
        FileLock::acquire(&path, Duration::ZERO).unwrap();
    }
}
