use eframe::egui;
use winreg::{enums::HKEY_CURRENT_USER, RegKey};
const KEY: &str = r"Software\PCSweep\Session";
#[derive(Clone, PartialEq)]
pub struct Appearance {
    pub scale: u32,
    pub bold: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            scale: 100,
            bold: false,
        }
    }
}
impl Appearance {
    pub fn load() -> Self {
        let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(KEY) else {
            return Self::default();
        };
        Self {
            scale: k
                .get_value::<u32, _>("TextScale")
                .ok()
                .filter(|n| [100, 110, 125, 150].contains(n))
                .unwrap_or(100),
            bold: k.get_value::<u32, _>("BoldText").ok() == Some(1),
        }
    }
    pub fn save(&self) {
        if let Ok((k, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY) {
            let _ = k.set_value("TextScale", &self.scale);
            let _ = k.set_value("BoldText", &u32::from(self.bold));
        }
    }
    pub fn apply(&self, ctx: &egui::Context) {
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new("pcsweep.text_scale"),
                self.scale as f32 / 100.0,
            );
            d.insert_temp(egui::Id::new("pcsweep.bold"), self.bold);
        });
    }
}
pub fn start_elevated() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(KEY)
        .and_then(|k| k.get_value::<u32, _>("StartElevated"))
        .ok()
        == Some(1)
}
pub fn set_start_elevated(on: bool) {
    if let Ok((k, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY) {
        let _ = k.set_value("StartElevated", &u32::from(on));
    }
}
