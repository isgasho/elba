//! Locking to make sure that multiple copies of `elba` don't clobber each other.
//! 
//! As it is currently designed, `elba` doesn't need to lock individual files. It does, however,
//! need to lock directories to prevent other processes from using them.

use std::{fs, io, path::{Path, PathBuf}};

/// A lock on a directory. This just generates a sibling file to the directory which indicates that
/// the directory is locked. 
#[derive(Debug)]
pub struct DirLock {
    path: PathBuf,
    lock_path: PathBuf,
}

impl DirLock {
    pub fn acquire<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = fs::canonicalize(path)?;
        let lock_path = { let mut p = path.clone(); p.set_extension("lock"); p };
        fs::OpenOptions::new().write(true).create_new(true).open(&lock_path).map(|_| DirLock { path, lock_path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}