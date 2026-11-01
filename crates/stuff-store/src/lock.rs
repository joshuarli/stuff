use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt;
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
        let file = open_lock_file(path)?;
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

    pub fn acquire_blocking(path: &Path) -> Result<Self, LockError> {
        let file = open_lock_file(path)?;
        file.lock().map_err(LockError::Io)?;
        Ok(Self { _file: file })
    }

    pub fn try_acquire(path: &Path) -> Result<Option<Self>, LockError> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File, LockError> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(LockError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_is_not_taken_over_when_its_file_names_a_dead_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".publish.lock");
        let _holder = FileLock::acquire(&path, Duration::ZERO).unwrap();
        std::fs::write(&path, "999999\n").unwrap();
        let second = FileLock::acquire(&path, Duration::from_millis(250));
        assert!(matches!(second, Err(LockError::TimedOut)), "{second:?}");
        assert!(matches!(FileLock::try_acquire(&path), Ok(None)));
    }

    #[test]
    fn dropping_the_holder_releases_the_lock_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".publish.lock");
        drop(FileLock::acquire(&path, Duration::ZERO).unwrap());
        assert!(path.exists());
        FileLock::acquire(&path, Duration::ZERO).unwrap();
        drop(FileLock::acquire_blocking(&path).unwrap());
        assert!(FileLock::try_acquire(&path).unwrap().is_some());
    }
}
