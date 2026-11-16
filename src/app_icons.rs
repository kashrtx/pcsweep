//! Native application artwork, read on bounded background jobs. Registry icon
//! references are data: nothing here starts an application or an uninstaller.
use crate::{task::Task, util, widgets};
use eframe::egui::{self, TextureHandle, Ui};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::c_void,
    fs,
    io::Read,
    mem::{size_of, zeroed},
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::SIZE,
        Graphics::Gdi::*,
        Storage::FileSystem::GetDriveTypeW,
        System::Com::{
            CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
        },
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

const ICON_SIZE: usize = 48;
const MAX_CACHED: usize = 512;
const MAX_QUEUED: usize = 128;
const BATCH_SIZE: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum IconSource {
    Desktop {
        display: String,
        executable: String,
        location: String,
        identity: String,
    },
    Store {
        family: String,
        location: String,
        identity: String,
    },
    Generic,
}
impl Default for IconSource {
    fn default() -> Self {
        Self::Generic
    }
}
impl IconSource {
    pub fn desktop(display: String, executable: String, location: String, version: &str) -> Self {
        // This constructor runs with inventory on its worker, never while
        // painting rows. In-place icon/executable replacements invalidate cache.
        let stamp = parse_display_icon(&display)
            .map(|(p, _)| p)
            .or_else(|| local_path(&executable));
        let stamp = stamp
            .filter(|p| on_local_drive(p))
            .and_then(|p| fs::metadata(p).ok())
            .map(|m| {
                let modified = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                format!("{}:{modified:?}", m.len())
            });
        Self::Desktop {
            display,
            executable,
            location,
            identity: format!("{version}:{stamp:?}"),
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn local_path(text: &str) -> Option<PathBuf> {
    let expanded = util::expand_env(text.trim().trim_matches('"'));
    if expanded.contains(['\0', '\r', '\n']) || expanded.len() > 32760 {
        return None;
    }
    let p = PathBuf::from(expanded);
    // Shell handlers for a disconnected UNC share can wait indefinitely. Icons
    // never require network locations or device namespaces.
    match p.components().next()? {
        Component::Prefix(prefix)
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
                && p.is_absolute() =>
        {
            Some(p)
        }
        _ => None,
    }
}
fn on_local_drive(path: &Path) -> bool {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let drive = match prefix.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
        _ => return false,
    };
    let root = wide(&format!("{}:\\", drive as char));
    // DRIVE_REMOTE, DRIVE_UNKNOWN and DRIVE_NO_ROOT_DIR are all unsuitable.
    matches!(unsafe { GetDriveTypeW(root.as_ptr()) }, 2 | 3 | 5 | 6)
}

/// DisplayIcon supports a quoted filename containing commas, followed by a
/// signed resource/index value. A negative value names a resource ID.
fn parse_display_icon(text: &str) -> Option<(PathBuf, i32)> {
    let text = text.trim().strip_prefix('@').unwrap_or(text.trim());
    let (name, index) = if let Some(rest) = text.strip_prefix('"') {
        let end = rest.find('"')?;
        let tail = rest[end + 1..].trim();
        let index = if tail.is_empty() {
            0
        } else {
            tail.strip_prefix(',')?.trim().parse().ok()?
        };
        (&rest[..end], index)
    } else if let Some((path, last)) = text.rsplit_once(',') {
        match last.trim().parse::<i32>() {
            Ok(index) => (path.trim(), index),
            Err(_) => (text, 0),
        }
    } else {
        (text, 0)
    };
    Some((local_path(name)?, index))
}

struct Icon(isize);
impl Drop for Icon {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                DestroyIcon(self.0);
            }
        }
    }
}
struct Bitmap(isize);
impl Drop for Bitmap {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                DeleteObject(self.0);
            }
        }
    }
}
struct Dc(isize);
impl Drop for Dc {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                DeleteDC(self.0);
            }
        }
    }
}
struct Selected {
    dc: isize,
    previous: isize,
}
impl Drop for Selected {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
        }
    }
}
struct ComApartment;
impl ComApartment {
    fn start() -> Option<Self> {
        (unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        } >= 0)
            .then_some(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

#[derive(Clone)]
struct Pixels {
    size: [usize; 2],
    rgba: Vec<u8>,
}
fn dib_info(width: i32, height: i32) -> BITMAPINFO {
    let mut info: BITMAPINFO = unsafe { zeroed() };
    info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    info
}
fn draw_background(icon: &Icon, color: u8) -> Option<Vec<u8>> {
    let dc = Dc(unsafe { CreateCompatibleDC(0) });
    if dc.0 == 0 {
        return None;
    }
    let mut bits = std::ptr::null_mut();
    let bitmap = Bitmap(unsafe {
        CreateDIBSection(
            dc.0,
            &dib_info(ICON_SIZE as i32, ICON_SIZE as i32),
            DIB_RGB_COLORS,
            &mut bits,
            0,
            0,
        )
    });
    if bitmap.0 == 0 || bits.is_null() {
        return None;
    }
    let previous = unsafe { SelectObject(dc.0, bitmap.0) };
    if previous == 0 || previous == -1 {
        return None;
    }
    let _selected = Selected { dc: dc.0, previous };
    {
        let buffer =
            unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), ICON_SIZE * ICON_SIZE * 4) };
        for pixel in buffer.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[color, color, color, 255]);
        }
    }
    if unsafe {
        DrawIconEx(
            dc.0,
            0,
            0,
            icon.0,
            ICON_SIZE as i32,
            ICON_SIZE as i32,
            0,
            0,
            DI_NORMAL,
        )
    } == 0
    {
        return None;
    }
    // CreateDIBSection shares these bits with GDI. Flush queued drawing before
    // the worker reads the pixels (required by the Windows API contract).
    if unsafe { GdiFlush() } == 0 {
        return None;
    }
    Some(unsafe {
        std::slice::from_raw_parts(bits.cast::<u8>(), ICON_SIZE * ICON_SIZE * 4).to_vec()
    })
}
fn icon_pixels(icon: &Icon) -> Option<Pixels> {
    // Rendering onto black and white recovers transparency for BOTH modern
    // alpha icons and old mask-only icons; old icons often have all-zero alpha.
    let black = draw_background(icon, 0)?;
    let white = draw_background(icon, 255)?;
    let mut rgba = Vec::with_capacity(black.len());
    for (b, w) in black.chunks_exact(4).zip(white.chunks_exact(4)) {
        let difference = (0..3)
            .map(|i| w[i].saturating_sub(b[i]) as u16)
            .sum::<u16>()
            / 3;
        let a = 255 - difference as u8;
        let straight = |v: u8| {
            if a == 0 {
                0
            } else {
                ((u32::from(v) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
            }
        };
        rgba.extend([straight(b[2]), straight(b[1]), straight(b[0]), a]);
    }
    Some(Pixels {
        size: [ICON_SIZE, ICON_SIZE],
        rgba,
    })
}
fn extract_reference(path: &Path, index: i32) -> Option<Pixels> {
    if !on_local_drive(path) || !path.is_file() {
        return None;
    }
    let path = wide(path.to_str()?);
    let mut handle = 0;
    let count =
        unsafe { ExtractIconExW(path.as_ptr(), index, &mut handle, std::ptr::null_mut(), 1) };
    let icon = Icon(handle);
    (count > 0 && icon.0 != 0)
        .then(|| icon_pixels(&icon))
        .flatten()
}
fn shell_file(path: &Path) -> Option<Pixels> {
    if !on_local_drive(path) || !path.is_file() {
        return None;
    }
    let name = wide(path.to_str()?);
    let mut info: SHFILEINFOW = unsafe { zeroed() };
    if unsafe {
        SHGetFileInfoW(
            name.as_ptr(),
            0,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    } == 0
    {
        return None;
    }
    icon_pixels(&Icon(info.hIcon))
}
fn generic_icon() -> Option<Pixels> {
    let mut info: SHSTOCKICONINFO = unsafe { zeroed() };
    info.cbSize = size_of::<SHSTOCKICONINFO>() as u32;
    if unsafe { SHGetStockIconInfo(SIID_APPLICATION, SHGSI_ICON | SHGSI_LARGEICON, &mut info) } < 0
    {
        return None;
    }
    icon_pixels(&Icon(info.hIcon))
}

#[repr(C)]
struct FactoryVtable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_image: unsafe extern "system" fn(*mut c_void, SIZE, u32, *mut isize) -> i32,
}
struct Factory(*mut c_void);
impl Factory {
    fn vtable(&self) -> &FactoryVtable {
        unsafe { &**(self.0.cast::<*const FactoryVtable>()) }
    }
}
impl Drop for Factory {
    fn drop(&mut self) {
        unsafe {
            (self.vtable().release)(self.0);
        }
    }
}
const IID_IMAGE_FACTORY: GUID = GUID {
    data1: 0xbcc18b79,
    data2: 0xba16,
    data3: 0x442f,
    data4: [0x80, 0xc4, 0x8a, 0x59, 0xc3, 0x0c, 0x46, 0x3b],
};
fn bitmap_pixels(bitmap: &Bitmap) -> Option<Pixels> {
    let mut object: BITMAP = unsafe { zeroed() };
    if unsafe {
        GetObjectW(
            bitmap.0,
            size_of::<BITMAP>() as i32,
            (&mut object as *mut BITMAP).cast(),
        )
    } == 0
    {
        return None;
    }
    let width = object.bmWidth;
    let height = object.bmHeight.checked_abs()?;
    if width <= 0 || height <= 0 || width > 256 || height > 256 {
        return None;
    }
    let dc = Dc(unsafe { CreateCompatibleDC(0) });
    if dc.0 == 0 {
        return None;
    }
    let mut info = dib_info(width, height);
    let mut bgra = vec![0u8; width as usize * height as usize * 4];
    if unsafe {
        GetDIBits(
            dc.0,
            bitmap.0,
            0,
            height as u32,
            bgra.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        )
    } != height
    {
        return None;
    }
    let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
    let mut rgba = Vec::with_capacity(ICON_SIZE * ICON_SIZE * 4);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let at = ((y * height as usize / ICON_SIZE) * width as usize
                + x * width as usize / ICON_SIZE)
                * 4;
            let p = &bgra[at..at + 4];
            let alpha = if has_alpha { p[3] } else { 255 };
            let straight = |v: u8| {
                if alpha == 0 {
                    0
                } else {
                    ((u32::from(v) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255) as u8
                }
            };
            rgba.extend([straight(p[2]), straight(p[1]), straight(p[0]), alpha]);
        }
    }
    Some(Pixels {
        size: [ICON_SIZE, ICON_SIZE],
        rgba,
    })
}
fn shell_package(family: &str, id: &str) -> Option<Pixels> {
    if !valid_identity(family) || !valid_identity(id) {
        return None;
    }
    shell_image(&format!("shell:AppsFolder\\{family}!{id}"))
}
fn shell_image(parsing_name: &str) -> Option<Pixels> {
    let name = wide(parsing_name);
    let mut pointer = std::ptr::null_mut();
    if unsafe {
        SHCreateItemFromParsingName(
            name.as_ptr(),
            std::ptr::null_mut(),
            &IID_IMAGE_FACTORY,
            &mut pointer,
        )
    } < 0
        || pointer.is_null()
    {
        return None;
    }
    let factory = Factory(pointer);
    let mut bitmap = 0;
    let result = unsafe {
        (factory.vtable().get_image)(
            factory.0,
            SIZE {
                cx: ICON_SIZE as i32,
                cy: ICON_SIZE as i32,
            },
            0x4,
            &mut bitmap,
        )
    };
    let bitmap = Bitmap(bitmap);
    if result < 0 || bitmap.0 == 0 {
        return None;
    }
    bitmap_pixels(&bitmap)
}
fn valid_identity(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 512
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

// Parse only Application Id attributes. This is not an XML interpreter: external
// entities, executable attributes and resource URI values are never evaluated.
fn application_ids(xml: &str) -> Vec<String> {
    let mut result = Vec::new();
    for (offset, _) in xml.match_indices("<Application") {
        let rest = &xml[offset + "<Application".len()..];
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let Some(end) = rest.find('>') else { continue };
        let mut tag = &rest[..end];
        while !tag.trim_start().is_empty() {
            tag = tag.trim_start();
            let Some(eq) = tag.find('=') else { break };
            let name = tag[..eq].trim();
            let value = tag[eq + 1..].trim_start();
            let Some(quote) = value.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
                break;
            };
            let value = &value[1..];
            let Some(end) = value.find(quote) else { break };
            if name == "Id" && valid_identity(&value[..end]) {
                result.push(value[..end].to_string());
                break;
            }
            tag = &value[end + 1..];
        }
        if result.len() >= 16 {
            break;
        }
    }
    result
}
fn store_ids(location: &str) -> Vec<String> {
    let Some(root) = local_path(location).filter(|p| on_local_drive(p)) else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(root.join("AppxManifest.xml")) else {
        return Vec::new();
    };
    let mut text = String::new();
    if file.take(1024 * 1024).read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    application_ids(&text)
}
fn location_executable(location: &str) -> Option<PathBuf> {
    let root = local_path(location).filter(|p| on_local_drive(p))?;
    let mut files: Vec<_> = fs::read_dir(root)
        .ok()?
        .take(96)
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                && p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy().to_ascii_lowercase();
                    !n.starts_with("unins") && !n.starts_with("setup")
                })
        })
        .collect();
    files.sort();
    files.into_iter().next()
}
fn load_icon(source: &IconSource) -> Option<Pixels> {
    match source {
        IconSource::Generic => generic_icon(),
        IconSource::Desktop {
            display,
            executable,
            location,
            ..
        } => parse_display_icon(display)
            .and_then(|(path, index)| extract_reference(&path, index))
            .or_else(|| {
                location_executable(location)
                    .and_then(|p| extract_reference(&p, 0).or_else(|| shell_file(&p)))
            })
            .or_else(|| {
                local_path(executable)
                    .and_then(|p| extract_reference(&p, 0).or_else(|| shell_file(&p)))
            }),
        IconSource::Store {
            family, location, ..
        } => {
            let mut ids = store_ids(location);
            if !ids.iter().any(|v| v == "App") {
                ids.push("App".into());
            }
            ids.into_iter().find_map(|id| shell_package(family, &id))
        }
    }
}

struct Cached {
    texture: Option<TextureHandle>,
    last_use: u64,
}
#[derive(Default)]
pub struct IconCache {
    entries: HashMap<IconSource, Cached>,
    queued: VecDeque<IconSource>,
    requested: HashSet<IconSource>,
    active: Vec<IconSource>,
    task: Task<Vec<(IconSource, Option<Pixels>)>>,
    tick: u64,
    #[cfg(test)]
    inert: bool,
}
impl IconCache {
    #[cfg(test)]
    pub fn preview_fixture() -> Self {
        Self {
            inert: true,
            ..Self::default()
        }
    }
    fn queue(&mut self, source: &IconSource) {
        #[cfg(test)]
        if self.inert {
            return;
        }
        if self.entries.contains_key(source)
            || self.requested.contains(source)
            || self.queued.len() >= MAX_QUEUED
        {
            return;
        }
        self.requested.insert(source.clone());
        self.queued.push_back(source.clone());
    }
    pub fn show(&mut self, ui: &mut Ui, source: &IconSource) {
        #[cfg(test)]
        if self.inert && !self.entries.contains_key(&IconSource::Generic) {
            let mut image = egui::ColorImage::new([16, 16], egui::Color32::TRANSPARENT);
            for y in 2..14 {
                for x in 2..14 {
                    image[(x, y)] = if y < 6 {
                        egui::Color32::from_rgb(91, 125, 230)
                    } else {
                        egui::Color32::from_rgb(66, 83, 142)
                    };
                }
            }
            let texture =
                ui.ctx()
                    .load_texture("fixture-app-icon", image, egui::TextureOptions::NEAREST);
            self.entries.insert(
                IconSource::Generic,
                Cached {
                    texture: Some(texture),
                    last_use: self.tick,
                },
            );
        }
        self.tick = self.tick.wrapping_add(1);
        self.queue(&IconSource::Generic);
        self.queue(source);
        let tick = self.tick;
        let texture = self
            .entries
            .get_mut(source)
            .and_then(|v| {
                v.last_use = tick;
                v.texture.clone()
            })
            .or_else(|| {
                self.entries
                    .get(&IconSource::Generic)
                    .and_then(|v| v.texture.clone())
            });
        let size = egui::vec2(28.0, 28.0);
        if let Some(texture) = texture {
            ui.add(egui::Image::new(&texture).fit_to_exact_size(size));
        } else {
            ui.allocate_ui_with_layout(
                size,
                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    ui.label(widgets::icon(
                        widgets::ic::APPS,
                        22.0,
                        crate::theme::TEXT_DIM,
                    ));
                },
            );
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.task.poll() {
            let pixels: HashMap<_, _> = result.unwrap_or_default().into_iter().collect();
            for source in self.active.drain(..) {
                self.requested.remove(&source);
                let texture = pixels.get(&source).and_then(Option::as_ref).map(|p| {
                    ctx.load_texture(
                        "installed-app-native-icon",
                        egui::ColorImage::from_rgba_unmultiplied(p.size, &p.rgba),
                        egui::TextureOptions::LINEAR,
                    )
                });
                self.entries.insert(
                    source,
                    Cached {
                        texture,
                        last_use: self.tick,
                    },
                );
            }
            self.trim();
        }
    }
    fn trim(&mut self) {
        while self.entries.len() > MAX_CACHED {
            let oldest = self
                .entries
                .iter()
                .filter(|(k, _)| !matches!(k, IconSource::Generic))
                .min_by_key(|(_, v)| v.last_use)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest {
                self.entries.remove(&key);
            } else {
                break;
            }
        }
    }
    pub fn start_pending(&mut self, ctx: &egui::Context) {
        #[cfg(test)]
        if self.inert {
            return;
        }
        if self.task.running() || self.queued.is_empty() {
            return;
        }
        self.active = self
            .queued
            .drain(..self.queued.len().min(BATCH_SIZE))
            .collect();
        let work = self.active.clone();
        self.task = Task::spawn(ctx, move || {
            let _apartment = ComApartment::start();
            work.into_iter()
                .map(|s| {
                    let p = load_icon(&s);
                    (s, p)
                })
                .collect()
        });
    }
    pub fn refresh(&mut self) {
        // Keep successful textures through refresh/filtering and retry failures.
        self.entries.retain(|_, v| v.texture.is_some());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_icon_parses_quotes_commas_environment_and_signed_resource_ids() {
        assert_eq!(
            parse_display_icon(r#""C:\Apps\A,B\app.exe",-123"#),
            Some((PathBuf::from(r"C:\Apps\A,B\app.exe"), -123))
        );
        assert_eq!(
            parse_display_icon(r"C:\Apps\A,B\app.ico"),
            Some((PathBuf::from(r"C:\Apps\A,B\app.ico"), 0))
        );
        assert_eq!(
            parse_display_icon(r#"@"C:\Apps\app.dll",2"#),
            Some((PathBuf::from(r"C:\Apps\app.dll"), 2))
        );
        let (expanded, resource) =
            parse_display_icon(r"%SystemRoot%\System32\shell32.dll,-154").unwrap();
        assert_eq!(
            expanded,
            PathBuf::from(std::env::var("SystemRoot").unwrap()).join(r"System32\shell32.dll")
        );
        assert_eq!(resource, -154);
        assert!(parse_display_icon(r#""C:\Apps\app.exe" /run"#).is_none());
        assert!(parse_display_icon(r"\\server\share\app.exe,0").is_none());
        assert!(parse_display_icon("C:\\app.exe\n,0").is_none());
        assert!(parse_display_icon(r"relative.exe,0").is_none());
    }
    #[test]
    fn package_manifest_ids_are_bounded_data_and_not_executable_attributes() {
        let ids = application_ids(
            r#"<Applications><Application Executable="not-run.exe" Id='Photos.App'><uap:VisualElements /></Application><Application Id="App2" Executable="also-not-run.exe"/></Applications>"#,
        );
        assert_eq!(ids, vec!["Photos.App", "App2"]);
        assert!(
            application_ids(r#"<Application Id="bad!id" /><Application Id="&external;"/>"#)
                .is_empty()
        );
    }
    #[test]
    fn requests_are_deduplicated_and_bounded_even_with_missing_icons() {
        let mut cache = IconCache::default();
        for i in 0..1000 {
            let source = IconSource::Store {
                family: format!("App{i}"),
                location: String::new(),
                identity: String::new(),
            };
            cache.queue(&source);
            cache.queue(&source);
        }
        assert_eq!(cache.queued.len(), MAX_QUEUED);
        assert_eq!(cache.requested.len(), MAX_QUEUED);
        for i in 0..700 {
            cache.entries.insert(
                IconSource::Store {
                    family: format!("App{i}"),
                    location: String::new(),
                    identity: String::new(),
                },
                Cached {
                    texture: None,
                    last_use: i,
                },
            );
        }
        cache.trim();
        assert_eq!(cache.entries.len(), MAX_CACHED);
    }
    #[test]
    fn native_stock_and_system_executable_icons_have_visible_pixels() {
        let _com = ComApartment::start();
        let stock = generic_icon().expect("Windows supplies a native application icon");
        assert_eq!(stock.rgba.len(), ICON_SIZE * ICON_SIZE * 4);
        assert!(stock.rgba.chunks_exact(4).any(|p| p[3] > 0));
        let windows = crate::platform::system_directory().unwrap();
        let icon = extract_reference(&windows.join("shell32.dll"), 0)
            .expect("Windows shell icon resource can be read without executing it");
        assert!(icon.rgba.chunks_exact(4).any(|p| p[3] > 0));
        let factory = shell_image(windows.join("shell32.dll").to_str().unwrap())
            .expect("native Shell image factory exposes its icon bitmap");
        assert_eq!(factory.size, [ICON_SIZE, ICON_SIZE]);
        assert!(factory.rgba.chunks_exact(4).any(|p| p[3] > 0));
    }
}
