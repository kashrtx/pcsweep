//! Local scan snapshots are a convenience, never authority to delete a file.
//! All I/O happens on workers. Atomic publication keeps the last valid snapshot
//! available after an interrupted write; links and oversized input are refused.
use crate::util;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA: u32 = 1;
const MAX_BYTES: u64 = 128 * 1024 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(1);
static WRITE_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    schema: u32,
    saved: u64,
    data: T,
}
#[derive(Deserialize)]
struct Header {
    schema: u32,
    saved: u64,
}
pub struct Stored<T> {
    pub saved: u64,
    pub data: T,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
pub fn saved_label(stamp: u64) -> String {
    chrono::DateTime::from_timestamp_millis(stamp.min(i64::MAX as u64) as i64)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%b %e, %Y at %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "an earlier session".into())
}
pub fn directory() -> PathBuf {
    #[cfg(not(test))]
    {
        util::log_path().with_file_name("scan-cache")
    }
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("pcsweep-test-scan-cache-{}", std::process::id()))
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 100
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        && !name.contains("..")
}
fn directories(dir: &Path, create: bool) -> Result<Vec<File>, String> {
    if !dir.is_absolute() {
        return Err("Scan cache needs an absolute local folder.".into());
    }
    let mut held = Vec::new();
    let mut paths: Vec<_> = dir.ancestors().collect();
    paths.reverse();
    for p in &paths {
        if !p.exists() {
            continue;
        }
        let f = OpenOptions::new()
            .access_mode(0x80)
            .share_mode(3)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .open(p)
            .map_err(|e| e.to_string())?;
        let md = f.metadata().map_err(|e| e.to_string())?;
        if !md.is_dir() || util::is_reparse(&md) {
            return Err("Scan cache cannot use links or junctions.".into());
        }
        held.push(f);
    }
    if create {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        // Reopen created ancestors before any cache file is touched.
        held.extend(directories(dir, false)?);
    } else if !dir.is_dir() {
        return Err("Scan cache folder is unavailable.".into());
    }
    Ok(held)
}
fn regular(file: &File) -> Result<(), String> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let md = file.metadata().map_err(|e| e.to_string())?;
    if !md.is_file() || util::is_reparse(&md) || md.len() > MAX_BYTES {
        return Err("Invalid or oversized scan cache.".into());
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0
        || info.nNumberOfLinks != 1
    {
        return Err("Scan cache cannot use hard links.".into());
    }
    Ok(())
}
pub fn load<T: DeserializeOwned>(name: &str) -> Result<Option<Stored<T>>, String> {
    load_in(&directory(), name)
}
fn load_in<T: DeserializeOwned>(dir: &Path, name: &str) -> Result<Option<Stored<T>>, String> {
    if !safe_name(name) {
        return Err("Invalid cache name.".into());
    }
    if !dir.exists() {
        return Ok(None);
    }
    let _held = directories(dir, false)?;
    let file = match OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .custom_flags(0x0020_0000)
        .open(dir.join(name))
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    regular(&file)?;
    let value: Envelope<T> = serde_json::from_reader(file.take(MAX_BYTES + 1))
        .map_err(|e| format!("Saved scan could not be read: {e}"))?;
    if value.schema != SCHEMA
        || value.saved == 0
        || value.saved > now().saturating_add(5 * 60 * 1000)
    {
        return Err("Saved scan has an unsupported version or date.".into());
    }
    Ok(Some(Stored {
        saved: value.saved,
        data: value.data,
    }))
}
struct Limited<'a> {
    file: &'a mut File,
    bytes: u64,
}
impl Write for Limited<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.saturating_add(bytes.len() as u64) > MAX_BYTES {
            return Err(std::io::Error::other("Scan is too large to save locally."));
        }
        let n = self.file.write(bytes)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
pub fn save<T: Serialize>(name: &str, saved: u64, data: &T) -> Result<(), String> {
    save_in(&directory(), name, saved, data)
}
fn save_in<T: Serialize>(dir: &Path, name: &str, saved: u64, data: &T) -> Result<(), String> {
    if !safe_name(name) {
        return Err("Invalid cache name.".into());
    }
    let _writer = WRITE_MUTEX
        .lock()
        .map_err(|_| "Saved scan writer is unavailable")?;
    let _held = directories(dir, true)?;
    // Only one process publishes cache changes at a time. Nothing in a cache is
    // worth delaying the UI or another process: a busy writer is reported.
    let lease = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(0x0020_0000)
        .open(dir.join("writer.lock"))
        .map_err(|e| e.to_string())?;
    regular(&lease)?;
    let target = dir.join(name);
    match OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .custom_flags(0x0020_0000)
        .open(&target)
    {
        Ok(f) => {
            regular(&f)?;
            if serde_json::from_reader::<_, Header>(f.take(MAX_BYTES + 1))
                .ok()
                .is_some_and(|h| h.schema == SCHEMA && h.saved > saved)
            {
                return Ok(());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let temp = dir.join(format!(
        "pending-{}-{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .share_mode(0)
            .custom_flags(0x0020_0000)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(
            Limited {
                file: &mut f,
                bytes: 0,
            },
            &Envelope {
                schema: SCHEMA,
                saved,
                data,
            },
        )
        .map_err(|e| e.to_string())?;
        f.flush()
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
        drop(f);
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let wide = |p: &Path| {
            p.as_os_str()
                .to_string_lossy()
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        if unsafe {
            MoveFileExW(
                wide(&temp).as_ptr(),
                wide(&target).as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn exists(name: &str) -> bool {
    safe_name(name) && directory().join(name).is_file()
}

pub fn file_size(name: &str) -> Result<Option<u64>, String> {
    if !safe_name(name) {
        return Err("Invalid cache name.".into());
    }
    let dir = directory();
    let _held = directories(&dir, false)?;
    let file = match OpenOptions::new()
        .read(true)
        .share_mode(1 | 4)
        .custom_flags(0x0020_0000)
        .open(dir.join(name))
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    regular(&file)?;
    Ok(Some(file.metadata().map_err(|e| e.to_string())?.len()))
}

pub fn prune_maps(keep: &std::collections::HashSet<String>) -> Result<(), String> {
    let dir = directory();
    let _held = directories(&dir, false)?;
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("map-")
            && name.ends_with(".json")
            && safe_name(&name)
            && !keep.contains(&name)
        {
            let md = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            // Leave a short grace period for another app instance publishing
            // a new immutable snapshot before its index is replaced.
            if md.is_file()
                && !util::is_reparse(&md)
                && md
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > 60)
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

impl crate::app::App {
    pub fn restore_scan_caches(&mut self, ctx: &eframe::egui::Context) {
        self.restore_analyzer_cache(ctx);
        self.restore_cleaner_cache(ctx);
        self.restore_duplicate_cache(ctx);
    }
    pub fn poll_scan_caches(&mut self, ctx: &eframe::egui::Context) {
        self.poll_analyzer_cache(ctx);
        self.poll_cleaner_cache(ctx);
        self.poll_duplicate_cache(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pcsweep-cache-{label}-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }
    #[test]
    fn cache_roundtrip_atomic_replacement_and_invalid_input() {
        let dir = temp("roundtrip");
        save_in(&dir, "test.json", now(), &vec!["first"]).unwrap();
        save_in(&dir, "test.json", now(), &vec!["second"]).unwrap();
        assert_eq!(
            load_in::<Vec<String>>(&dir, "test.json")
                .unwrap()
                .unwrap()
                .data,
            vec!["second"]
        );
        assert!(save_in(&dir, "../escape", now(), &0).is_err());
        fs::write(dir.join("test.json"), b"{broken").unwrap();
        assert!(load_in::<Vec<String>>(&dir, "test.json").is_err());
        fs::write(
            dir.join("test.json"),
            br#"{"schema":999,"saved":1,"data":[]}"#,
        )
        .unwrap();
        assert!(load_in::<Vec<String>>(&dir, "test.json").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_delayed_old_snapshot_cannot_replace_a_newer_scan() {
        let dir = temp("generation");
        save_in(&dir, "test.json", 200, &vec!["new scan"]).unwrap();
        save_in(&dir, "test.json", 100, &vec!["old metadata job"]).unwrap();
        let restored = load_in::<Vec<String>>(&dir, "test.json").unwrap().unwrap();
        assert_eq!(restored.saved, 200);
        assert_eq!(restored.data, vec!["new scan"]);
        save_in(&dir, "test.json", 200, &vec!["same scan, updated choices"]).unwrap();
        assert_eq!(
            load_in::<Vec<String>>(&dir, "test.json")
                .unwrap()
                .unwrap()
                .data,
            vec!["same scan, updated choices"]
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cache_rejects_hardlinks_and_oversized_files() {
        let dir = temp("hardlink");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("original"), b"important").unwrap();
        fs::hard_link(dir.join("original"), dir.join("test.json")).unwrap();
        assert!(save_in(&dir, "test.json", now(), &0).is_err());
        assert!(load_in::<u32>(&dir, "test.json").is_err());
        assert_eq!(fs::read(dir.join("original")).unwrap(), b"important");
        fs::remove_file(dir.join("test.json")).unwrap();
        File::create(dir.join("test.json"))
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(load_in::<u32>(&dir, "test.json").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
