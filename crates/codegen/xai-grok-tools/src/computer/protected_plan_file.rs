//! Symlink-safe access to the session-owned `plan.md`.
//!
//! Plan Mode auto-approves writes to exactly one file. That makes the file
//! itself a security boundary: generic path-based writes must not be allowed
//! to follow a planted symlink (including a symlink in a parent component).
//! Unix uses an `openat(2)` directory-fd walk with `O_NOFOLLOW`; writes land
//! through a same-directory temporary file and `renameat(2)`, so a final-path
//! replacement race can never redirect bytes into another file.
//! Non-Unix platforms reject links observed during validation and use an
//! atomic temporary-file replacement where the platform supports it, but the
//! standard-library fallback does not provide Unix's descriptor-relative
//! parent-walk guarantee against a concurrent reparse-point swap.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::types::{AsyncFileSystem, ComputerError};

/// Wrap a normal tool filesystem and route one exact plan path through the
/// protected host-local boundary.
///
/// The wrapper is installed only by the session layer. Unit-test/tool-only
/// callers that merely provide a display `PlanFilePath` retain their injected
/// filesystem behavior.
pub struct GuardedPlanFileSystem {
    inner: Arc<dyn AsyncFileSystem>,
    protected_path: PathBuf,
}

impl GuardedPlanFileSystem {
    pub fn new(inner: Arc<dyn AsyncFileSystem>, protected_path: PathBuf) -> Self {
        Self {
            inner,
            protected_path,
        }
    }

    pub fn protects(&self, path: &Path) -> bool {
        path == self.protected_path
    }
}

#[async_trait::async_trait]
impl AsyncFileSystem for GuardedPlanFileSystem {
    async fn read_file(&self, path: &Path) -> Result<Vec<u8>, ComputerError> {
        if self.protects(path) {
            return read(path).await.map_err(ComputerError::from);
        }
        self.inner.read_file(path).await
    }

    async fn write_file(&self, path: &Path, data: &[u8]) -> Result<(), ComputerError> {
        if self.protects(path) {
            return write(path, data).await.map_err(ComputerError::from);
        }
        self.inner.write_file(path, data).await
    }

    async fn delete_file(&self, path: &Path) -> Result<(), ComputerError> {
        if self.protects(path) {
            return Err(ComputerError::io_with_kind(
                "protected plan file deletion is not allowed",
                io::ErrorKind::PermissionDenied,
            ));
        }
        self.inner.delete_file(path).await
    }
}

pub async fn read(path: &Path) -> io::Result<Vec<u8>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_blocking(&path))
        .await
        .map_err(io::Error::other)?
}

pub fn read_blocking(path: &Path) -> io::Result<Vec<u8>> {
    platform::read(path)
}

pub async fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    let path = path.to_path_buf();
    let data = data.to_vec();
    tokio::task::spawn_blocking(move || platform::write(&path, &data))
        .await
        .map_err(io::Error::other)?
}

#[cfg(unix)]
mod platform {
    use std::ffi::{CString, OsStr};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::{Component, Path};

    fn denied(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, message.into())
    }

    fn component_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
    }

    fn openat(directory: &File, name: &OsStr, flags: i32, mode: libc::mode_t) -> io::Result<File> {
        let name = component_name(name)?;
        // SAFETY: `directory` is live, `name` is NUL terminated, and a mode is
        // supplied for the only callers that may include `O_CREAT`.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful openat transfers one owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_root() -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).custom_flags(
            libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        );
        options.open("/")
    }

    fn open_parent(path: &Path, create: bool) -> io::Result<(File, &OsStr)> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "protected plan path must be absolute",
            ));
        }
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
        let mut directory = open_root()?;
        for component in parent.components() {
            match component {
                Component::RootDir => continue,
                Component::Normal(component) => {
                    let flags = libc::O_RDONLY
                        | libc::O_DIRECTORY
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK;
                    match openat(&directory, component, flags, 0) {
                        Ok(next) => directory = next,
                        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                            let component = component_name(component)?;
                            // SAFETY: the parent fd and component are valid.
                            let rc = unsafe {
                                libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o700)
                            };
                            if rc != 0 {
                                let mkdir_error = io::Error::last_os_error();
                                if mkdir_error.kind() != io::ErrorKind::AlreadyExists {
                                    return Err(mkdir_error);
                                }
                            }
                            directory = openat(
                                &directory,
                                OsStr::from_bytes(component.as_bytes()),
                                flags,
                                0,
                            )?;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Component::CurDir => {}
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "protected plan path is not normalized",
                    ));
                }
            }
        }
        Ok((directory, name))
    }

    fn validate_regular(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(denied("protected plan path is not a regular file"));
        }
        if metadata.nlink() != 1 {
            return Err(denied("protected plan file has multiple hard links"));
        }
        Ok(())
    }

    fn validate_destination(directory: &File, name: &OsStr) -> io::Result<()> {
        let name = component_name(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: pointers are valid and `stat` is initialized on success.
        let rc = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: fstatat succeeded.
        let stat = unsafe { stat.assume_init() };
        if (stat.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Err(denied(
                "protected plan destination is a symlink or non-regular file",
            ));
        }
        if stat.st_nlink != 1 {
            return Err(denied("protected plan destination has multiple hard links"));
        }
        Ok(())
    }

    fn unlinkat(directory: &File, name: &OsStr) {
        if let Ok(name) = component_name(name) {
            // SAFETY: best-effort cleanup of a name in the held directory.
            unsafe {
                libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
            }
        }
    }

    pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
        let (directory, name) = open_parent(path, false)?;
        let mut file = openat(
            &directory,
            name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )?;
        validate_regular(&file)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub(super) fn write(path: &Path, data: &[u8]) -> io::Result<()> {
        let (directory, name) = open_parent(path, true)?;
        // Reject a symlink/hard-link that was already present. The second check
        // below catches replacements during the write; `renameat` itself never
        // follows the final component.
        validate_destination(&directory, name)?;

        let temp_name = format!(
            ".{}.grok-protected-{}-{}",
            name.to_string_lossy(),
            std::process::id(),
            uuid::Uuid::now_v7()
        );
        let temp_name = OsStr::new(&temp_name);
        let mut temp = openat(
            &directory,
            temp_name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )?;
        let result = (|| {
            validate_regular(&temp)?;
            temp.write_all(data)?;
            temp.sync_all()?;
            validate_destination(&directory, name)?;
            let old = component_name(temp_name)?;
            let new = component_name(name)?;
            // SAFETY: both names are relative to the held directory fd.
            if unsafe {
                libc::renameat(
                    directory.as_raw_fd(),
                    old.as_ptr(),
                    directory.as_raw_fd(),
                    new.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            directory.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            unlinkat(&directory, temp_name);
        }
        result
    }
}

#[cfg(not(unix))]
mod platform {
    // This fallback is intentionally not described as equivalent to the Unix
    // implementation. `std::fs` has no handle-relative parent walk, so each
    // component is rejected when observed as a link, but an attacker able to
    // race namespace changes can still create a validate-then-open window.
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read as _, Write as _};
    use std::path::{Component, Path};

    fn denied(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, message.into())
    }

    fn validate_components(path: &Path, create: bool) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "protected plan path must be absolute",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
        let mut current = std::path::PathBuf::new();
        for component in parent.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => current.push(component.as_os_str()),
                Component::Normal(name) => {
                    current.push(name);
                    match fs::symlink_metadata(&current) {
                        Ok(metadata) => {
                            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                                return Err(denied(
                                    "protected plan parent is a symlink or non-directory",
                                ));
                            }
                        }
                        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                            fs::create_dir(&current)?;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "protected plan path is not normalized",
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_final(path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(denied(
                        "protected plan destination is a symlink or non-regular file",
                    ));
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
        validate_components(path, false)?;
        validate_final(path)?;
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub(super) fn write(path: &Path, data: &[u8]) -> io::Result<()> {
        validate_components(path, true)?;
        validate_final(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
        let temp = path.with_file_name(format!(
            ".{}.grok-protected-{}-{}",
            name.to_string_lossy(),
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(data)?;
            file.sync_all()?;
            validate_components(path, false)?;
            validate_final(path)?;
            // Rust's Windows implementation uses replace-existing rename
            // semantics for regular files. A raced symlink is replaced as a
            // directory entry, never followed for data writes.
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn protected_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        // macOS exposes the default temp directory through `/var`, which is a
        // symlink. Canonicalize the fixture root so this test exercises the
        // protected file itself rather than correctly rejecting that parent.
        let root = dunce::canonicalize(dir.path()).unwrap();
        let path = root.join("nested").join("plan.md");
        write(&path, b"one").await.unwrap();
        assert_eq!(read(&path).await.unwrap(), b"one");
        write(&path, b"two").await.unwrap();
        assert_eq!(read(&path).await.unwrap(), b"two");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_final_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let target = root.join("secret");
        std::fs::write(&target, b"secret").unwrap();
        let plan = root.join("plan.md");
        symlink(&target, &plan).unwrap();

        let error = write(&plan, b"overwrite").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&target).unwrap(), b"secret");
        assert!(read(&plan).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_parent_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let target_dir = root.join("outside");
        std::fs::create_dir(&target_dir).unwrap();
        let linked_parent = root.join("session");
        symlink(&target_dir, &linked_parent).unwrap();
        let plan = linked_parent.join("plan.md");

        assert!(write(&plan, b"overwrite").await.is_err());
        assert!(!target_dir.join("plan.md").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_hard_link_without_touching_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let target = root.join("secret");
        std::fs::write(&target, b"secret").unwrap();
        let plan = root.join("plan.md");
        std::fs::hard_link(&target, &plan).unwrap();

        let error = write(&plan, b"overwrite").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&target).unwrap(), b"secret");
        assert!(read(&plan).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn raced_final_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let target = root.join("secret");
        std::fs::write(&target, b"secret").unwrap();
        let plan = root.join("plan.md");
        std::fs::write(&plan, b"old").unwrap();

        for _ in 0..64 {
            let _ = std::fs::remove_file(&plan);
            symlink(&target, &plan).unwrap();
            let _ = write(&plan, b"new").await;
            assert_eq!(std::fs::read(&target).unwrap(), b"secret");
            if std::fs::symlink_metadata(&plan)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                std::fs::remove_file(&plan).unwrap();
            }
            std::fs::write(&plan, b"old").unwrap();
        }
    }
}
