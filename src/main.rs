// Hide the console window in release builds.
#![windows_subsystem = "windows"]

mod ads;
mod analyzer;
mod app;
mod app_icons;
mod app_packages;
mod appearance;
mod apps;
mod background;
mod cleaner;
mod dashboard;
mod dns;
mod duplicates;
mod file_changes;
mod instance;
mod journal;
mod leftovers;
mod network;
mod platform;
mod power;
mod preferences;
mod privacy;
mod processes;
mod quick_search;
mod recovery;
mod running;
mod scan_cache;
mod security;
mod speed;
mod startup;
mod storage;
mod system;
mod task;
mod theme;
mod tools;
mod tray;
#[cfg(test)]
mod ui_preview;
mod update_control;
mod update_install;
mod updates;
mod util;
mod widgets;
mod windows_controls;
mod winfile;

use eframe::egui;

fn main() -> Result<(), eframe::Error> {
    if let Some(code) = dns::run_helper_mode() {
        std::process::exit(code);
    }
    if let Some(code) = update_install::run_helper_mode() {
        std::process::exit(code);
    }
    instance::wait_for_parent();
    let _instance = match instance::acquire() {
        Ok(Some(guard)) => guard,
        Ok(None) => return Ok(()),
        Err(error) => {
            instance::show_error(&error);
            return Ok(());
        }
    };
    if instance::elevate_on_start() {
        return Ok(());
    }
    update_install::pin_running_app();
    recovery::warm_desktop();
    // Deep folder trees are scanned recursively in parallel; give worker threads
    // plenty of stack so nothing can overflow on unusual directory layouts.
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .build_global();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PCSweep")
            .with_inner_size(app::saved_window_size())
            .with_min_inner_size([900.0, 520.0])
            .with_clamp_size_to_monitor_size(true)
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "PCSweep",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// Use the same embedded artwork for the taskbar and Explorer. The procedural
/// icon is a fallback if the embedded image cannot be decoded.
fn app_icon() -> egui::IconData {
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        return icon;
    }
    let n: u32 = 64;
    let half = n as f32 / 2.0;
    let radius = 15.0;
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            // Rounded-square mask.
            let dx = ((fx - half).abs() - (half - radius)).max(0.0);
            let dy = ((fy - half).abs() - (half - radius)).max(0.0);
            let d = (dx * dx + dy * dy).sqrt();
            let alpha = (radius - d + 0.5).clamp(0.0, 1.0);
            // Diagonal gradient: signal blue to fresh green.
            let t = (fx + fy) / (2.0 * n as f32);
            let mut c = [
                143.0 + (79.0 - 143.0) * t,
                168.0 + (209.0 - 168.0) * t,
                255.0 + (165.0 - 255.0) * t,
            ];
            // A white ring with a dot: a stylized "clean disk".
            let cd = ((fx - half).powi(2) + (fy - half).powi(2)).sqrt();
            let ring = (1.0 - ((cd - 17.0).abs() - 2.5).clamp(0.0, 1.0)).clamp(0.0, 1.0);
            let dot = (5.5 - cd + 0.5).clamp(0.0, 1.0);
            let w = ring.max(dot);
            for ch in c.iter_mut() {
                *ch += (255.0 - *ch) * w;
            }
            rgba.extend_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, (alpha * 255.0) as u8]);
        }
    }
    egui::IconData {
        rgba,
        width: n,
        height: n,
    }
}
