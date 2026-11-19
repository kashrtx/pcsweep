//! Read-only, bounded Windows evidence. Never enables auditing or creates a USN journal.
use crate::{
    app::{App, Page},
    platform,
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, Label, ScrollArea, Ui};
use serde::Deserialize;
use std::{
    collections::{HashMap, VecDeque},
    mem::{size_of, zeroed},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    Storage::FileSystem::*,
    System::{Ioctl::*, IO::DeviceIoControl},
};

const MAX_ROWS: usize = 2000;
const REASONS: u32 = 0x100 | 0x200 | 0x1000 | 0x2000 | 1 | 2 | 4 | 0x8000 | 0x800;
#[derive(Clone, Default)]
struct Change {
    time: String,
    path: String,
    event: String,
    actor: String,
    parent: u64,
    name: String,
}
#[derive(Default)]
struct Evidence {
    rows: Vec<Change>,
    note: String,
    resume: Option<(String, i64, u64)>,
}
struct Handle(isize);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn last_error() -> String {
    std::io::Error::last_os_error().to_string()
}
fn volume_letter(value: &str) -> Result<char, String> {
    let bytes = value.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        Ok((bytes[0] as char).to_ascii_uppercase())
    } else {
        Err("Select a local drive letter reported by Windows.".into())
    }
}
fn reason_text(reason: u32) -> String {
    [
        (0x100, "Created"),
        (0x200, "Deleted"),
        (0x1000, "Renamed from"),
        (0x2000, "Renamed to"),
        (7, "Content changed"),
        (0x8000, "Metadata changed"),
        (0x800, "Permissions changed"),
    ]
    .into_iter()
    .filter(|(mask, _)| reason & mask != 0)
    .map(|(_, name)| name)
    .collect::<Vec<_>>()
    .join(" · ")
}
fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn parse_page(bytes: &[u8]) -> Result<(i64, Vec<Change>, usize), String> {
    if bytes.len() < 8 {
        return Err("Windows returned an incomplete change-journal page.".into());
    }
    let next = u64_at(bytes, 0) as i64;
    let mut at = 8;
    let mut rows = Vec::new();
    let mut skipped = 0;
    while at < bytes.len() {
        if bytes.len() - at < 8 {
            return Err("Incomplete filesystem change record.".into());
        }
        let length = u32_at(bytes, at) as usize;
        if length < 8 || length > bytes.len() - at {
            return Err("Invalid filesystem change record length.".into());
        }
        let b = &bytes[at..at + length];
        at += length;
        if u16_at(b, 4) != 2 {
            skipped += 1;
            continue;
        }
        if length < 60 {
            return Err("Incomplete NTFS version 2 record.".into());
        }
        let name_len = u16_at(b, 56) as usize;
        let name_at = u16_at(b, 58) as usize;
        if name_len % 2 != 0
            || name_at < 60
            || name_at.checked_add(name_len).is_none_or(|end| end > length)
        {
            return Err("Invalid NTFS filename bounds.".into());
        }
        let reason = u32_at(b, 40);
        if reason & REASONS == 0 {
            continue;
        }
        let name = String::from_utf16_lossy(
            &b[name_at..name_at + name_len]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        );
        let stamp = u64_at(b, 32) as i64;
        let time = chrono::DateTime::from_timestamp(
            stamp.div_euclid(10_000_000) - 11_644_473_600,
            (stamp.rem_euclid(10_000_000) * 100) as u32,
        )
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "Time unavailable".into());
        rows.push(Change {
            time,
            parent: u64_at(b, 16),
            name,
            event: reason_text(reason),
            actor: "Not recorded by NTFS".into(),
            ..Default::default()
        });
    }
    Ok((next, rows, skipped))
}
fn parent_path(volume: &Handle, id: u64) -> Option<String> {
    let desc = FILE_ID_DESCRIPTOR {
        dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: id as i64 },
    };
    let h = unsafe {
        OpenFileById(
            volume.0,
            &desc,
            0,
            7,
            std::ptr::null(),
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let h = Handle(h);
    let mut text = vec![0u16; 32768];
    let n =
        unsafe { GetFinalPathNameByHandleW(h.0, text.as_mut_ptr(), text.len() as u32, 0) } as usize;
    if n == 0 || n >= text.len() {
        None
    } else {
        Some(
            String::from_utf16_lossy(&text[..n])
                .trim_start_matches(r"\\?\")
                .to_string(),
        )
    }
}
fn read_ntfs(drive: String, resume: Option<(i64, u64)>) -> Result<Evidence, String> {
    if !crate::system::is_admin() {
        return Err("Reading the existing NTFS journal requires administrator access. No auditing setting was changed.".into());
    }
    let letter = volume_letter(&drive)?;
    let name: Vec<u16> = format!(r"\\.\{letter}:")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x8000_0000,
            7,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(format!("Cannot read this volume: {}", last_error()));
    }
    let volume = Handle(h);
    let mut info: USN_JOURNAL_DATA_V0 = unsafe { zeroed() };
    let mut n = 0;
    if unsafe {
        DeviceIoControl(
            volume.0,
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            &mut info as *mut _ as *mut _,
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            &mut n,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!("An existing readable NTFS change journal is unavailable: {}. PCSweep does not create one.",last_error()));
    }
    if (n as usize) < size_of::<USN_JOURNAL_DATA_V0>() {
        return Err("Incomplete change-journal metadata.".into());
    }
    let mut request = READ_USN_JOURNAL_DATA_V0 {
        StartUsn: match resume {
            Some((cursor, journal)) if journal == info.UsnJournalID && cursor >= info.FirstUsn && cursor <= info.NextUsn => cursor,
            Some(_) => return Err("The retained Windows journal changed. Read Windows records again to start a fresh sample.".into()),
            None => info.FirstUsn,
        },
        ReasonMask: REASONS,
        ReturnOnlyOnClose: 1,
        Timeout: 0,
        BytesToWaitFor: 0,
        UsnJournalID: info.UsnJournalID,
    };
    let mut buffer = vec![0u8; 64 * 1024];
    let mut rows = VecDeque::new();
    let mut skipped = 0;
    let start = Instant::now();
    let mut bytes = 0usize;
    while request.StartUsn < info.NextUsn
        && bytes < 64 * 1024 * 1024
        && start.elapsed() < Duration::from_secs(8)
    {
        let mut received = 0;
        if unsafe {
            DeviceIoControl(
                volume.0,
                FSCTL_READ_USN_JOURNAL,
                &request as *const _ as *const _,
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut received,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "Journal reading stopped: {}. The journal may have changed or access was refused.",
                last_error()
            ));
        }
        let received = received as usize;
        if received > buffer.len() {
            return Err("Invalid journal output size.".into());
        }
        bytes += received;
        let (next, events, unsupported) = parse_page(&buffer[..received])?;
        skipped += unsupported;
        for event in events {
            rows.push_back(event);
            if rows.len() > MAX_ROWS {
                rows.pop_front();
            }
        }
        if next <= request.StartUsn {
            break;
        }
        request.StartUsn = next;
    }
    let mut cache: HashMap<u64, Option<String>> = HashMap::new();
    let mut rows: Vec<Change> = rows.into_iter().rev().collect();
    for row in &mut rows {
        // Path resolution is best-effort and deliberately bounded; deleted or
        // renamed parents cannot provide an authoritative historical full path.
        let parent = if cache.len() < 256 || cache.contains_key(&row.parent) {
            cache
                .entry(row.parent)
                .or_insert_with(|| parent_path(&volume, row.parent))
                .clone()
        } else {
            None
        };
        row.path = parent
            .map(|p| format!("{}\\{}", p.trim_end_matches('\\'), row.name))
            .unwrap_or_else(|| format!("[parent ID {:x}]\\{}", row.parent, row.name));
    }
    let more = request.StartUsn < info.NextUsn;
    Ok(Evidence{rows,resume:more.then(||(drive,request.StartUsn,info.UsnJournalID)),note:format!("Retained NTFS journal for {letter}:. Up to {MAX_ROWS} last records in this bounded batch, newest first. {} Older events can expire. {skipped} unsupported records skipped. Paths use current parent locations where resolvable, not guaranteed historical full paths. This is change evidence, not file recovery or program attribution.",if more {"More retained records remain; use Read next batch to continue toward the present."} else {"Reached the end captured when this read began."})})
}
const AUDIT_SCRIPT: &str = r#"
$r=@(Get-WinEvent -FilterHashtable @{LogName='Security';Id=4663} -MaxEvents 500 -ErrorAction Stop | ForEach-Object {
 $x=[xml]$_.ToXml();$d=@{};foreach($v in $x.Event.EventData.Data){$d[[string]$v.Name]=[string]$v.'#text'}
 $mask=[Convert]::ToUInt32(($d.AccessMask -replace '^0x',''),16)
 if($d.ObjectType -eq 'File' -and ($mask -band 0x10116) -ne 0){[pscustomobject]@{time=$_.TimeCreated.ToString('yyyy-MM-dd HH:mm:ss');path=$d.ObjectName;event='File access audited (write/delete rights; not proof of deletion)';actor=$(if($d.ProcessName){$d.ProcessName}else{'Not recorded'})}}
});ConvertTo-Json -InputObject $r -Compress
"#;
fn read_audit() -> Result<Evidence, String> {
    let output = platform::powershell(AUDIT_SCRIPT, Duration::from_secs(20))?;
    #[derive(Deserialize)]
    struct Row {
        time: String,
        path: String,
        event: String,
        actor: String,
    }
    let rows: Vec<Row> = serde_json::from_str(&output).map_err(|e| e.to_string())?;
    Ok(Evidence{resume:None,rows:rows.into_iter().map(|r|Change{time:r.time,path:r.path,event:r.event,actor:r.actor,..Default::default()}).collect(),note:"Existing Security event 4663 evidence only. It requires prior file auditing and suitable access-control auditing on the files. PCSweep does not enable it. A recorded access right is not proof of the final operation; missing events do not mean nothing changed.".into()})
}
#[derive(Default)]
pub struct FileChangesState {
    job: Task<Result<Evidence, String>>,
    evidence: Option<Evidence>,
    error: Option<String>,
    drive: String,
    filter: String,
    audit: bool,
}
impl App {
    pub fn poll_file_changes(&mut self) {
        if let Some(r) = self.file_changes.job.poll() {
            match r.and_then(|r| r) {
                Ok(e) => self.file_changes.evidence = Some(e),
                Err(e) => self.file_changes.error = Some(e),
            }
        }
    }
    pub fn page_file_changes(&mut self, ui: &mut Ui) {
        widgets::page_header(ui,"Windows file changes","Inspect existing Windows records to understand changes beyond PCSweep's own cleanup history.");
        ui.label(widgets::dim("Windows File History is an optional backup feature. The NTFS journal is a separate, partial change record; it does not identify which program made a change."));
        let selected_before = (self.file_changes.audit, self.file_changes.drive.clone());
        let mut read = false;
        let mut resume_read = None;
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!self.file_changes.job.running(), |ui| {
                ui.selectable_value(&mut self.file_changes.audit, false, "Filesystem changes");
                ui.selectable_value(
                    &mut self.file_changes.audit,
                    true,
                    "Existing audit evidence",
                );
                if self.file_changes.drive.is_empty() {
                    self.file_changes.drive = "C:".into();
                }
                if !self.file_changes.audit {
                    egui::ComboBox::from_id_salt("changes_drive")
                        .selected_text(&self.file_changes.drive)
                        .show_ui(ui, |ui| {
                            for d in &self.sys.drives {
                                let s = d.letter();
                                if volume_letter(&s).is_ok() {
                                    ui.selectable_value(&mut self.file_changes.drive, s.clone(), s);
                                }
                            }
                        });
                }
            });
            read = ui
                .add_enabled(
                    !self.file_changes.job.running(),
                    widgets::primary("Read Windows records"),
                )
                .clicked();
            if ui.add(widgets::small("PCSweep cleanup history")).clicked() {
                self.page = Page::Activity;
                self.history.open = true;
                self.history.refresh(ui.ctx());
            }
            if ui.add(widgets::small("Windows backup settings")).clicked() {
                util::open_uri("ms-settings:backup");
            }
            if self.file_changes.job.running() {
                ui.spinner();
                ui.label("Reading a bounded sample…");
            }
        });
        if selected_before != (self.file_changes.audit, self.file_changes.drive.clone()) {
            self.file_changes.evidence = None;
            self.file_changes.error = None;
        }
        if let Some(e) = &self.file_changes.error {
            ui.colored_label(color_warning(), e);
            ui.label(widgets::dim("Unavailable records are not proof that no files changed. Auditing must already have been enabled for access evidence."));
        }
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.text_edit_singleline(&mut self.file_changes.filter);
        });
        if let Some(e) = &self.file_changes.evidence {
            ui.label(widgets::dim(&e.note));
            if let Some(cursor) = &e.resume {
                if ui
                    .add_enabled(
                        !self.file_changes.job.running(),
                        widgets::small("Read next retained batch"),
                    )
                    .clicked()
                {
                    resume_read = Some(cursor.clone());
                }
            }
            let query = self.file_changes.filter.to_lowercase();
            let rows: Vec<_> = e
                .rows
                .iter()
                .filter(|r| {
                    query.is_empty()
                        || format!("{} {} {}", r.path, r.event, r.actor)
                            .to_lowercase()
                            .contains(&query)
                })
                .collect();
            ui.label(widgets::faint(format!("{} records shown", rows.len())));
            ScrollArea::vertical()
                .id_salt("file_change_records")
                .auto_shrink([false, false])
                .show_rows(ui, 88.0, rows.len(), |ui, range| {
                    for r in &rows[range] {
                        ui.vertical(|ui| {
                            ui.label(widgets::strong(format!("{} · {}", r.time, r.event), 14.0));
                            ui.add(Label::new(widgets::dim(&r.path)).truncate())
                                .on_hover_text(&r.path)
                                .context_menu(|ui| {
                                    if ui.button("Copy path / reference").clicked() {
                                        ui.output_mut(|o| o.copied_text = r.path.clone());
                                        ui.close_menu();
                                    }
                                });
                            ui.add(
                                Label::new(widgets::faint(format!(
                                    "Program evidence: {}",
                                    r.actor
                                )))
                                .truncate(),
                            )
                            .on_hover_text(&r.actor);
                        });
                        ui.separator();
                    }
                });
        } else {
            ui.label(widgets::dim("Read existing records to begin. This view cannot reconstruct a complete past history or restore permanently deleted files."));
        }
        if read || resume_read.is_some() {
            self.file_changes.error = None;
            self.file_changes.evidence = None;
            let (drive, continuation) = resume_read
                .map(|(drive, usn, journal)| (drive, Some((usn, journal))))
                .unwrap_or_else(|| (self.file_changes.drive.clone(), None));
            let audit = self.file_changes.audit;
            self.file_changes.job = Task::spawn(ui.ctx(), move || {
                if audit {
                    read_audit()
                } else {
                    read_ntfs(drive, continuation)
                }
            });
        }
    }
}
#[cfg(test)]
pub(crate) fn preview_fixture() -> FileChangesState {
    FileChangesState{drive:"C:".into(),evidence:Some(Evidence{rows:vec![Change{time:"2026-11-15 12:00:00".into(),path:r"C:\Example\report.txt".into(),event:"Content changed".into(),actor:"Not recorded by NTFS".into(),..Default::default()}],note:"Bounded retained Windows records. Program attribution is unavailable in NTFS records.".into(),resume:None}),..Default::default()}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_volume_and_malformed_records() {
        for v in ["C:;whoami", "C:\\path", "\\\\server", ""] {
            assert!(volume_letter(v).is_err());
        }
        assert_eq!(volume_letter("c:").unwrap(), 'C');
        assert!(parse_page(&[0; 7]).is_err());
        let mut b = vec![0; 16];
        b[8..12].copy_from_slice(&100u32.to_le_bytes());
        assert!(parse_page(&b).is_err());
    }
    #[test]
    fn parses_native_record_without_inventing_actor() {
        let mut b = vec![0u8; 72];
        b[..8].copy_from_slice(&100i64.to_le_bytes());
        b[8..12].copy_from_slice(&64u32.to_le_bytes());
        b[12..14].copy_from_slice(&2u16.to_le_bytes());
        b[48..52].copy_from_slice(&0x200u32.to_le_bytes());
        b[64..66].copy_from_slice(&4u16.to_le_bytes());
        b[66..68].copy_from_slice(&60u16.to_le_bytes());
        b[68..72].copy_from_slice(&[b'a', 0, b'b', 0]);
        let (_, r, _) = parse_page(&b).unwrap();
        assert_eq!(r[0].name, "ab");
        assert_eq!(r[0].event, "Deleted");
        assert_eq!(r[0].actor, "Not recorded by NTFS");
        b[64..66].copy_from_slice(&5u16.to_le_bytes());
        assert!(parse_page(&b).is_err());
    }
    #[test]
    fn audit_script_parses_without_reading_security_log() {
        let script=format!("$t=$null;$e=$null;[void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$t,[ref]$e);if($e.Count){{throw ($e|Out-String)}}",platform::ps_quote(AUDIT_SCRIPT));
        platform::powershell(&script, Duration::from_secs(15)).unwrap();
    }
}
