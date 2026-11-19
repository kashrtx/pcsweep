//! Pin local Win32 paths while verifying, moving, or launching an executable.
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of, zeroed};
use std::os::windows::{
    ffi::{OsStrExt, OsStringExt},
    fs::OpenOptionsExt,
    io::AsRawHandle,
};
use std::path::{Component, Path, PathBuf};
use windows_sys::Win32::Foundation::{GENERIC_READ, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    FileRenameInfo, GetFileInformationByHandle, GetFinalPathNameByHandleW,
    SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

pub fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn handle(file: &File) -> HANDLE {
    file.as_raw_handle() as HANDLE
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    use windows_sys::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
    let strip = |p: &Path| {
        let mut chars: Vec<_> = p.as_os_str().encode_wide().collect();
        if chars.starts_with(&[92, 92, 63, 92]) {
            chars.drain(..4);
        }
        chars
    };
    let (a, b) = (strip(a), strip(b));
    (unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) })
        == CSTR_EQUAL
}

fn regular_local(path: &Path) -> bool {
    crate::util::fixed_local(path)
        && path.components().all(|part| match part {
            Component::Normal(name) => !name.encode_wide().any(|c| c == 0 || c == b':' as u16),
            Component::CurDir | Component::ParentDir => false,
            _ => true,
        })
}

pub fn open(path: &Path, access: u32, share: u32) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(access)
        .share_mode(share)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

pub fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0; 32_768];
    let n = unsafe {
        GetFinalPathNameByHandleW(handle(file), buffer.as_mut_ptr(), buffer.len() as u32, 0)
    } as usize;
    if n == 0 || n >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(std::ffi::OsString::from_wide(&buffer[..n]).into())
}

pub fn lock_directories(dir: &Path) -> io::Result<Vec<File>> {
    if !regular_local(dir) {
        return Err(io::Error::other(
            "A regular folder on a fixed local drive is required.",
        ));
    }
    let parents: Vec<_> = dir.ancestors().filter(|p| p.has_root()).collect();
    parents
        .into_iter()
        .rev()
        .map(|path| {
            // List access makes the share mode effective. Open every component
            // without following reparse points, then keep each handle until
            // the operation is finished. Windows may use valid 8.3 aliases
            // or spell a volume root differently when it describes a handle.
            let file = open(
                path,
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
            )?;
            let info = file.metadata()?;
            if !info.is_dir() || crate::util::is_reparse(&info) {
                return Err(io::Error::other(format!(
                    "An ordinary local folder is required. Cannot use {}.",
                    path.display()
                )));
            }
            Ok(file)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Deserialize, serde::Serialize)]
pub struct Stamp {
    volume: u32,
    id: u64,
    created: u64,
    written: u64,
    len: u64,
}

impl Stamp {
    /// Windows can reuse the creation time of a replaced name during a rename.
    /// Use this with a content digest when following our own executable move.
    pub fn same_file(self, other: Self) -> bool {
        self.volume == other.volume && self.id == other.id
    }
}

pub fn stamp(file: &File) -> io::Result<Stamp> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(handle(file), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let md = file.metadata()?;
    if !md.is_file() || crate::util::is_reparse(&md) || info.nNumberOfLinks != 1 {
        return Err(io::Error::other(
            "The executable is a link or is not an ordinary file.",
        ));
    }
    let pair = |high, low| (u64::from(high) << 32) | u64::from(low);
    Ok(Stamp {
        volume: info.dwVolumeSerialNumber,
        id: pair(info.nFileIndexHigh, info.nFileIndexLow),
        created: pair(
            info.ftCreationTime.dwHighDateTime,
            info.ftCreationTime.dwLowDateTime,
        ),
        written: pair(
            info.ftLastWriteTime.dwHighDateTime,
            info.ftLastWriteTime.dwLowDateTime,
        ),
        len: md.len(),
    })
}

pub fn read_locked(path: &Path, extra_access: u32) -> io::Result<File> {
    if !regular_local(path) {
        return Err(io::Error::other(
            "The executable must be on a fixed local drive.",
        ));
    }
    let file = open(path, GENERIC_READ | extra_access, FILE_SHARE_READ)?;
    stamp(&file)?;
    Ok(file)
}

pub fn rename_locked(file: &File, destination: &Path) -> io::Result<()> {
    // A handle selects the source. A no-replace rename cannot overwrite a
    // different file inserted at the destination after verification.
    let name: Vec<_> = destination.as_os_str().encode_wide().collect();
    let offset = offset_of!(FILE_RENAME_INFO, FileName);
    let bytes = (offset + (name.len() + 1) * size_of::<u16>()).max(size_of::<FILE_RENAME_INFO>());
    let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).FileNameLength = (name.len() * size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        if SetFileInformationByHandle(handle(file), FileRenameInfo, info.cast(), bytes as u32) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
