use crate::theme::{self, Skin};
use eframe::egui;
use winreg::{enums::HKEY_CURRENT_USER, RegKey};
const KEY: &str = r"Software\PCSweep\Session";
#[derive(Clone, PartialEq)]
pub struct Appearance {
    pub scale: u32,
    pub bold: bool,
    pub skin: Skin,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            scale: 100,
            bold: false,
            skin: Skin::default(),
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
            skin: k
                .get_value::<String, _>("Skin")
                .ok()
                .filter(|name| name.len() <= 32)
                .map(|name| Skin::from_key(&name))
                .unwrap_or_default(),
        }
    }
    pub fn save(&self) {
        if let Ok((k, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(KEY) {
            let _ = k.set_value("TextScale", &self.scale);
            let _ = k.set_value("BoldText", &u32::from(self.bold));
            let _ = k.set_value("Skin", &self.skin.key());
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
        theme::select(ctx, self.skin);
    }

    pub fn theme_picker(&mut self, ui: &mut egui::Ui) {
        ui.label(crate::widgets::strong("Appearance", 17.0));
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("pcsweep-skin")
                .selected_text(self.skin.name())
                .width(170.0)
                .show_ui(ui, |ui| {
                    for skin in Skin::ALL {
                        ui.selectable_value(&mut self.skin, skin, skin.name());
                    }
                });
            for color in self.skin.samples() {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                ui.painter().rect(
                    rect,
                    theme::corners(5.0),
                    color,
                    egui::Stroke::new(1.0_f32, theme::color_outline()),
                );
            }
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
