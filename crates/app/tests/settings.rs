//! The Settings window (M1 plan section 7.1), through the UI: File ▸
//! Settings… opens it, each change reaches the player's config at once,
//! a cache a flag set stays the flag's, and Restore defaults puts back
//! the player's defaults but for the Files pane's folder.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use egui::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use spherecut_app::settings::DecoderSetting;
use spherecut_app::{Settings, SpherecutApp};
use spherecut_media::backend::{FfmpegBackend, HwKind};
use spherecut_playback::budget::MIB;
use spherecut_playback::{CacheBudget, DecoderPreference, PlayerConfig};
use spherecut_render::Filter;

const SIZE: [f32; 2] = [1200.0, 760.0];

/// The grid clip's app, its settings as its config has them: software
/// decoding and a 256 MiB cache.
fn app(gpu: &spherecut_render::Gpu, cache_override: Option<&str>) -> SpherecutApp {
    let mut parts = common::parts(gpu, Arc::new(FfmpegBackend::new()));
    parts.settings = Settings {
        last_dir: Some("/videos".into()),
        cache_mib: Some(256),
        decoder: DecoderSetting::Software,
        ..Settings::default()
    };
    parts.cache_override = cache_override.map(str::to_owned);
    parts.open = vec![common::grid_clip().to_path_buf()];
    SpherecutApp::new(parts)
}

fn config(h: &Harness<'_, SpherecutApp>) -> PlayerConfig {
    h.state().state.player.as_ref().unwrap().config()
}

fn open_settings(h: &mut Harness<'_, SpherecutApp>) {
    h.get_by_label("File").click();
    h.step();
    h.step();
    h.get_by_label("Settings…").click();
    h.step();
    h.step();
    assert!(h.state().state.settings_open);
}

fn set(h: &mut Harness<'_, SpherecutApp>, label: &str, value: f64) {
    h.get_by_role_and_label(Role::SpinButton, label)
        .set_value_accesskit(value);
    h.step();
    h.step();
}

#[test]
fn each_setting_reaches_the_player_at_once() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(app(&gpu, None), &rs, SIZE, 1.0);
    h.step();
    assert!(h.query_by_label("GPU slots").is_none(), "closed at first");
    open_settings(&mut h);

    set(&mut h, "GPU slots", 3.0);
    assert_eq!(h.state().state.settings.gpu_slots, 3);
    assert_eq!(config(&h).gpu_slots, 3);
    // Past the range, the range's end.
    set(&mut h, "GPU slots", 9.0);
    assert_eq!(config(&h).gpu_slots, 6);

    set(&mut h, "A/V offset", -40.0);
    assert_eq!(h.state().state.settings.av_offset_ms, -40);
    assert_eq!(config(&h).av_offset_ms, -40);

    set(&mut h, "Frame cache", 1024.0);
    assert_eq!(h.state().state.settings.cache_mib, Some(1024));
    assert_eq!(config(&h).cache, CacheBudget::Bytes(1024 * MIB));
    // The upload thread takes the new budget, and the window says so.
    common::step_until(
        &mut h,
        std::time::Duration::from_secs(10),
        "the budget in force",
        |a| {
            a.state
                .snapshot
                .budget
                .is_some_and(|b| b.bytes == 1024 * MIB)
        },
    );
    h.step();
    let _ = h.get_by_label("in force: 1024 MiB");
    // Automatic again.
    h.get_by_label("Automatic").click();
    h.step();
    h.step();
    assert_eq!(h.state().state.settings.cache_mib, None);
    assert_eq!(config(&h).cache, CacheBudget::Auto);

    h.get_by_role_and_label(Role::ComboBox, "Decoder").click();
    h.step();
    h.step();
    h.get_by_label("VA-API only").click();
    h.step();
    h.step();
    assert_eq!(h.state().state.settings.decoder, DecoderSetting::VaApi);
    assert_eq!(config(&h).decoder, DecoderPreference::Only(HwKind::VaApi));

    // lavapipe has anisotropic filtering.
    assert_eq!(h.state().state.view.filter, Filter::Anisotropic);
    h.get_by_label("Anisotropic").click();
    h.step();
    h.step();
    assert!(!h.state().state.settings.anisotropic);
    assert_eq!(h.state().state.view.filter, Filter::Isotropic);

    h.get_by_label("Shown (F3)").click();
    h.step();
    h.step();
    assert!(h.state().state.settings.overlay);
    assert!(h.state().state.overlay.last.is_some());

    // Everything back, but the folder.
    h.get_by_label("Restore defaults").click();
    h.step();
    h.step();
    let s = &h.state().state.settings;
    assert_eq!(
        *s,
        Settings {
            last_dir: Some("/videos".into()),
            ..Settings::default()
        }
    );
    let c = config(&h);
    let d = PlayerConfig::default();
    assert_eq!(
        (c.cache, c.decoder, c.gpu_slots, c.av_offset_ms),
        (d.cache, d.decoder, d.gpu_slots, d.av_offset_ms)
    );
    assert_eq!(h.state().state.view.filter, Filter::Anisotropic);

    // The window's close button.
    h.get_by_label("Close window").click();
    h.step();
    h.step();
    assert!(!h.state().state.settings_open);
    assert!(h.query_by_label("GPU slots").is_none());
}

#[test]
fn a_cache_the_command_line_set_stays_its_own() {
    let gpu = common::gpu();
    let rs = common::render_state(&gpu);
    let mut h = common::harness(app(&gpu, Some("--cache-mb")), &rs, SIZE, 1.0);
    h.step();
    open_settings(&mut h);
    common::step_until(
        &mut h,
        std::time::Duration::from_secs(10),
        "the budget in force",
        |a| a.state.snapshot.budget.is_some(),
    );
    h.step();
    let _ = h.get_by_label("set by --cache-mb: 256 MiB");
    // The setting's field is there, and off.
    let field = h.get_by_role_and_label(Role::SpinButton, "Frame cache");
    assert!(field.accesskit_node().is_disabled());
    // Another setting leaves the flag's cache alone.
    set(&mut h, "GPU slots", 5.0);
    assert_eq!(config(&h).gpu_slots, 5);
    assert_eq!(config(&h).cache, CacheBudget::Bytes(256 * MIB));
}
