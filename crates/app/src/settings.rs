//! The settings (M1 plan sections 7.1 and 8.2): what the app keeps
//! between runs in eframe's storage, and the Settings window that changes
//! them. The player takes a change at once: the cache's budget and the
//! slot count as the upload thread gets to them, the A/V offset from the
//! next frame, and the decoder for the files opened after.
//!
//! `--cache-mb` and `SPHERECUT_FRAME_CACHE_MIB` win over the cache
//! setting; while one of them is in force the window says so and leaves
//! the cache alone.

use std::path::PathBuf;

use spherecut_media::backend::HwKind;
use spherecut_playback::budget::MIB;
use spherecut_playback::slots::{MAX_SLOTS, MIN_SLOTS};
use spherecut_playback::{CacheBudget, DecoderPreference, PlayerConfig};
use spherecut_render::Filter;

use crate::app::AppState;

/// The A/V offset the window allows, either way, in milliseconds.
pub const AV_OFFSET_MAX: i32 = 500;

/// The cache sizes the window allows, in MiB.
pub const CACHE_MIB: std::ops::RangeInclusive<u64> = 256..=65_536;

/// The decoder setting: [`DecoderPreference`], as it is stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecoderSetting {
    /// From doctor's checks, falling back to software.
    #[default]
    Auto,
    Software,
    VaApi,
    Vulkan,
}

impl DecoderSetting {
    pub const ALL: [DecoderSetting; 4] = [
        DecoderSetting::Auto,
        DecoderSetting::Software,
        DecoderSetting::VaApi,
        DecoderSetting::Vulkan,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DecoderSetting::Auto => "Automatic",
            DecoderSetting::Software => "Software",
            DecoderSetting::VaApi => "VA-API only",
            DecoderSetting::Vulkan => "Vulkan Video only",
        }
    }

    pub fn preference(self) -> DecoderPreference {
        match self {
            DecoderSetting::Auto => DecoderPreference::Auto,
            DecoderSetting::Software => DecoderPreference::Software,
            DecoderSetting::VaApi => DecoderPreference::Only(HwKind::VaApi),
            DecoderSetting::Vulkan => DecoderPreference::Only(HwKind::Vulkan),
        }
    }
}

/// What the app keeps between runs (eframe persistence). A field a
/// stored copy lacks takes its default, and one out of range is brought
/// into it ([`Settings::sanitized`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The Files pane's directory.
    pub last_dir: Option<PathBuf>,
    /// The anisotropic filter, else the isotropic one.
    pub anisotropic: bool,
    /// The overlay (F3).
    pub overlay: bool,
    /// The frame cache's budget in MiB, or `None` for the automatic one.
    pub cache_mib: Option<u64>,
    pub decoder: DecoderSetting,
    /// GPU slots, [`MIN_SLOTS`] to [`MAX_SLOTS`].
    pub gpu_slots: u8,
    /// Video this much ahead of the audio, behind it when negative
    /// ([`PlayerConfig::av_offset_ms`]).
    pub av_offset_ms: i32,
}

impl Default for Settings {
    fn default() -> Self {
        let config = PlayerConfig::default();
        Settings {
            last_dir: None,
            anisotropic: true,
            overlay: false,
            cache_mib: None,
            decoder: DecoderSetting::Auto,
            gpu_slots: config.gpu_slots,
            av_offset_ms: config.av_offset_ms,
        }
    }
}

impl Settings {
    /// The cache's budget these settings ask for.
    pub fn cache(&self) -> CacheBudget {
        self.cache_mib
            .map_or(CacheBudget::Auto, |mib| CacheBudget::Bytes(mib * MIB))
    }

    /// `config` with these settings: the decoder, the slots, the A/V
    /// offset, and the cache unless a flag or the environment chose it
    /// (`keep_cache`).
    pub fn apply(&self, config: &mut PlayerConfig, keep_cache: bool) {
        if !keep_cache {
            config.cache = self.cache();
        }
        config.decoder = self.decoder.preference();
        config.gpu_slots = self.gpu_slots;
        config.av_offset_ms = self.av_offset_ms;
    }

    /// Each value brought into the range the window allows, as a stored
    /// copy, edited by hand or from another version, may not be.
    pub fn sanitized(mut self) -> Settings {
        self.cache_mib = self
            .cache_mib
            .map(|m| m.clamp(*CACHE_MIB.start(), *CACHE_MIB.end()));
        self.gpu_slots = self.gpu_slots.clamp(MIN_SLOTS, MAX_SLOTS);
        self.av_offset_ms = self.av_offset_ms.clamp(-AV_OFFSET_MAX, AV_OFFSET_MAX);
        self
    }
}

/// The Settings window, while it is open.
pub fn window(s: &mut AppState, ctx: &egui::Context) {
    let mut open = s.settings_open;
    let mut changed = false;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| changed |= rows(s, ui));
            ui.separator();
            if ui.button("Restore defaults").clicked() {
                s.settings = Settings {
                    last_dir: s.settings.last_dir.clone(),
                    ..Settings::default()
                };
                changed = true;
            }
        });
    s.settings_open = open;
    if changed {
        s.apply_settings();
    }
}

/// The window's rows. Whether any changed.
fn rows(s: &mut AppState, ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    let set = &mut s.settings;

    let label = ui.label("Frame cache").id;
    ui.vertical(|ui| {
        ui.add_enabled_ui(s.cache_override.is_none(), |ui| {
            ui.horizontal(|ui| {
                let mut auto = set.cache_mib.is_none();
                if ui.checkbox(&mut auto, "Automatic").changed() {
                    set.cache_mib = if auto {
                        None
                    } else {
                        // From the budget in force, as a start.
                        let now = s.snapshot.budget.map_or(1024, |b| b.bytes / MIB);
                        Some(now.clamp(*CACHE_MIB.start(), *CACHE_MIB.end()))
                    };
                    changed = true;
                }
                if let Some(mib) = &mut set.cache_mib {
                    changed |= ui
                        .add(
                            egui::DragValue::new(mib)
                                .range(CACHE_MIB)
                                .speed(16.0)
                                .suffix(" MiB"),
                        )
                        .on_hover_text("The decoded frames kept in memory")
                        .labelled_by(label)
                        .changed();
                }
            });
        });
        let in_force = s
            .snapshot
            .budget
            .map_or_else(|| "-".into(), |b| format!("{} MiB", b.bytes / MIB));
        let note = match &s.cache_override {
            Some(by) => format!("set by {by}: {in_force}"),
            None => format!("in force: {in_force}"),
        };
        ui.weak(note);
    });
    ui.end_row();

    let label = ui.label("Decoder").id;
    ui.vertical(|ui| {
        egui::ComboBox::from_id_salt("decoder")
            .selected_text(set.decoder.label())
            .show_ui(ui, |ui| {
                for d in DecoderSetting::ALL {
                    changed |= ui
                        .selectable_value(&mut set.decoder, d, d.label())
                        .changed();
                }
            })
            .response
            .labelled_by(label);
        ui.weak("for the files opened from now on");
    });
    ui.end_row();

    let label = ui.label("GPU slots").id;
    changed |= ui
        .add(egui::DragValue::new(&mut set.gpu_slots).range(MIN_SLOTS..=MAX_SLOTS))
        .on_hover_text("Frames on the GPU, ready to show")
        .labelled_by(label)
        .changed();
    ui.end_row();

    let label = ui.label("A/V offset").id;
    changed |= ui
        .add(
            egui::DragValue::new(&mut set.av_offset_ms)
                .range(-AV_OFFSET_MAX..=AV_OFFSET_MAX)
                .suffix(" ms"),
        )
        .on_hover_text("Video this much ahead of the audio; negative for behind")
        .labelled_by(label)
        .changed();
    ui.end_row();

    ui.label("Filtering");
    let can = s.gpu.caps.anisotropy;
    let mut aniso = set.anisotropic && can;
    if ui
        .add_enabled(can, egui::Checkbox::new(&mut aniso, "Anisotropic"))
        .changed()
    {
        set.anisotropic = aniso;
        changed = true;
    }
    ui.end_row();

    ui.label("Overlay");
    changed |= ui.checkbox(&mut set.overlay, "Shown (F3)").changed();
    ui.end_row();
    changed
}

/// The filter the settings ask for, as far as the GPU has it.
pub fn filter(settings: &Settings, anisotropy: bool) -> Filter {
    if settings.anisotropic && anisotropy {
        Filter::Anisotropic
    } else {
        Filter::Isotropic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_copy_missing_fields_or_out_of_range_still_loads() {
        // As slice 7 stored them, without the fields slice 8 added.
        let old: Settings =
            serde_json::from_str(r#"{"last_dir":"/v","anisotropic":false,"overlay":true}"#)
                .unwrap();
        assert_eq!(
            old,
            Settings {
                last_dir: Some("/v".into()),
                anisotropic: false,
                overlay: true,
                ..Settings::default()
            }
        );
        let wild: Settings =
            serde_json::from_str(r#"{"cache_mib":1,"gpu_slots":99,"av_offset_ms":-9999}"#).unwrap();
        let wild = wild.sanitized();
        assert_eq!(wild.cache_mib, Some(256));
        assert_eq!(wild.gpu_slots, MAX_SLOTS);
        assert_eq!(wild.av_offset_ms, -AV_OFFSET_MAX);
        // And back, the same.
        let s = Settings {
            cache_mib: Some(2048),
            decoder: DecoderSetting::VaApi,
            gpu_slots: 5,
            av_offset_ms: -40,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert!(
            serde_json::to_string(&s)
                .unwrap()
                .contains(r#""decoder":"va_api""#)
        );
    }

    #[test]
    fn the_defaults_are_the_players() {
        let mut c = PlayerConfig {
            cache: CacheBudget::Bytes(1),
            ..PlayerConfig::default()
        };
        Settings::default().apply(&mut c, false);
        assert_eq!(
            format!("{c:?}"),
            format!("{:?}", PlayerConfig::default()),
            "the settings' defaults are the player's"
        );
        let s = Settings {
            cache_mib: Some(300),
            decoder: DecoderSetting::Software,
            gpu_slots: 6,
            av_offset_ms: 25,
            ..Settings::default()
        };
        let mut c = PlayerConfig::default();
        s.apply(&mut c, true);
        assert_eq!(c.cache, CacheBudget::Auto, "a flag's cache stays");
        assert_eq!(c.decoder, DecoderPreference::Software);
        assert_eq!((c.gpu_slots, c.av_offset_ms), (6, 25));
        s.apply(&mut c, false);
        assert_eq!(c.cache, CacheBudget::Bytes(300 * MIB));
    }
}
