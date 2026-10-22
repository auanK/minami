use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::MinamiError;

pub(crate) fn output_destination(output: &Path) -> Result<PathBuf, MinamiError> {
    let resource = output.display().to_string();
    let name = output.file_name().ok_or_else(|| {
        crate::epub::invalid(&resource, "output must name a new file or directory")
    })?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent =
        fs::canonicalize(parent).map_err(|e| io_error("opening output parent", &resource, e))?;
    if !parent.is_dir() {
        return Err(io_error(
            "opening output parent",
            &resource,
            io::Error::new(
                io::ErrorKind::NotADirectory,
                "output parent is not a directory",
            ),
        ));
    }
    let destination = parent.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(io_error(
                "creating output",
                &resource,
                io::Error::new(io::ErrorKind::AlreadyExists, "output already exists"),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error("checking output", &resource, e)),
    }
    Ok(destination)
}

#[cfg(windows)]
pub(crate) fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if from[..from.len() - 1].contains(&0) || to[..to.len() - 1].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in publication path",
        ));
    }
    // SAFETY: both pointers reference live NUL-terminated UTF-16 buffers. Zero flags forbid replacement and cross-volume copying.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(Into::into)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
pub(crate) fn rename_new(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace publication is supported on Windows and Linux/Android",
    ))
}

pub(crate) fn io_error(operation: &'static str, resource: &str, source: io::Error) -> MinamiError {
    MinamiError::Io {
        operation,
        resource: resource.into(),
        source,
    }
}
