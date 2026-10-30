//! The transport bar and scrubbing (M1 plan section 7.5).

use std::time::{Duration, Instant};

use spherecut_core::{Rational, Rounding, Time};
use spherecut_media::backend::{SeekMode, VideoStreamInfo};
use spherecut_playback::Player;

use crate::app::AppState;
use crate::view::Preset;

/// The keys, for the Help menu.
pub const KEYS_HELP: &str = "Space or K: play or pause\n\
    Right or .: next frame;  Left or ,: previous frame\n\
    Shift+Right and Shift+Left: a second on or back\n\
    Home and End: the first and last frames\n\
    Drag: look around (level; with Ctrl, free)\n\
    Scroll or Ctrl+scroll: zoom about the pointer;  + and -: zoom\n\
    1, 2, 3: Rect, Fisheye, Tiny planet\n\
    R: look forward, level;  F3: the overlay";

/// How long the pointer rests on the scrub bar before the frame under it
/// is sought exactly.
pub const EXACT_AFTER: Duration = Duration::from_millis(150);

/// `t` as `HH:MM:SS.mmm`, rounded down to the millisecond.
pub fn format_time(t: Time) -> String {
    let ms = t
        .to_pts(
            Rational::new(1, 1000).unwrap_or(Rational::ONE),
            Rounding::Floor,
        )
        .unwrap_or(0)
        .max(0);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The frame shown at `t`, counted from 0, when frames come at a
/// constant rate.
pub fn frame_number(t: Time, info: Option<&VideoStreamInfo>) -> Option<i64> {
    let rate = info?.frame_rate()?;
    rate.time_to_frame(t, Rounding::Floor).ok()
}

/// The time of the last frame of a clip lasting `duration`: one tick of
/// the stream's time base before its end, where an exact seek finds the
/// last frame shown (a millisecond, without the stream's).
pub fn last_frame_time(duration: Time, info: Option<&VideoStreamInfo>) -> Time {
    let tick = info
        .map(|i| i.time_base)
        .unwrap_or_else(|| Rational::new(1, 1000).unwrap_or(Rational::ONE));
    duration
        .checked_sub(Time::seconds(tick))
        .unwrap_or(Time::ZERO)
        .max(Time::ZERO)
}

/// A drag of the scrub bar.
#[derive(Debug, Clone, Copy)]
struct Drag {
    at: Time,
    moved: Instant,
    exact_sent: bool,
    /// Playback ran when the drag began, and goes on after it.
    resume: bool,
}

/// Scrubbing: keyframe seeks while the pointer moves, the latest
/// superseding the one before (the player stops it), and an exact seek
/// once it rests [`EXACT_AFTER`] or lets go. Playback pauses meanwhile,
/// so the audio is silent (audio scrubbing is M5's).
#[derive(Debug, Default)]
pub struct Scrub {
    drag: Option<Drag>,
}

impl Scrub {
    pub fn pending(&self) -> bool {
        self.drag.is_some()
    }

    /// The pointer is at clip time `at`.
    pub fn move_to(&mut self, player: Option<&Player>, at: Time, playing: bool, now: Instant) {
        let Some(p) = player else { return };
        match &mut self.drag {
            Some(d) if d.at == at => {}
            Some(d) => {
                d.at = at;
                d.moved = now;
                d.exact_sent = false;
                p.seek_at(at, SeekMode::Keyframe, now);
            }
            None => {
                if playing {
                    p.pause_at(now);
                }
                self.drag = Some(Drag {
                    at,
                    moved: now,
                    exact_sent: false,
                    resume: playing,
                });
                p.seek_at(at, SeekMode::Keyframe, now);
            }
        }
    }

    /// The exact seek, once the pointer has rested.
    pub fn settle(&mut self, player: Option<&Player>, now: Instant) {
        let (Some(p), Some(d)) = (player, &mut self.drag) else {
            return;
        };
        if !d.exact_sent && now.duration_since(d.moved) >= EXACT_AFTER {
            d.exact_sent = true;
            p.seek_at(d.at, SeekMode::Exact, now);
        }
    }

    /// The pointer let go.
    pub fn release(&mut self, player: Option<&Player>, now: Instant) {
        let (Some(p), Some(d)) = (player, self.drag.take()) else {
            return;
        };
        if !d.exact_sent {
            p.seek_at(d.at, SeekMode::Exact, now);
        }
        if d.resume {
            p.play_at(now);
        }
    }
}

/// The bar under the viewer: the buttons and time, the scrub bar, and
/// the projection buttons with the view's numbers.
pub fn bar_ui(s: &mut AppState, ui: &mut egui::Ui, now: Instant) {
    let shown = s.snapshot.shown.first().copied().flatten().map(|f| f.time);
    let at = if s.snapshot.playing || shown.is_none() {
        s.snapshot.clock
    } else {
        shown.unwrap_or(s.snapshot.clock)
    };
    let duration = s.duration();
    ui.horizontal(|ui| {
        let label = if s.snapshot.want_play {
            "Pause"
        } else {
            "Play"
        };
        if ui
            .button(label)
            .on_hover_text("Play or pause (Space, K)")
            .clicked()
        {
            s.toggle_play();
        }
        if ui
            .button("Prev")
            .on_hover_text("Previous frame (Left)")
            .clicked()
        {
            s.step(-1);
        }
        if ui
            .button("Next")
            .on_hover_text("Next frame (Right)")
            .clicked()
        {
            s.step(1);
        }
        ui.monospace(format!(
            "{} / {}",
            format_time(at),
            duration.map_or_else(|| "--:--:--.---".to_owned(), format_time)
        ));
        if let Some(n) = frame_number(at, s.stream0()) {
            ui.monospace(format!("#{n}"));
        }
    });
    scrub_bar(s, ui, at, duration, now);
    ui.horizontal(|ui| {
        let t = ui.input(|i| i.time);
        for p in Preset::ALL {
            if ui.selectable_label(s.view.preset == p, p.label()).clicked() {
                s.view.switch_to(p, t);
            }
        }
        ui.separator();
        ui.monospace(format!("FOV {:.0}°", s.view.lens.hfov.to_degrees()));
        let ypr = s.view.ypr();
        ui.monospace(format!(
            "Y {:.1}°  P {:.1}°  R {:.1}°",
            ypr.yaw.to_degrees(),
            ypr.pitch.to_degrees(),
            ypr.roll.to_degrees()
        ));
        if ui
            .button("Reset")
            .on_hover_text("Look forward, level (R)")
            .clicked()
        {
            s.view.reset();
        }
    });
}

/// The scrub bar: the cached spans shaded, the playhead, and the drag.
fn scrub_bar(s: &mut AppState, ui: &mut egui::Ui, at: Time, duration: Option<Time>, now: Instant) {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y * 0.6);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Slider, ui.is_enabled(), "Scrub bar")
    });
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);
    let Some(d) = duration.filter(|d| *d > Time::ZERO) else {
        return;
    };
    let secs = d.to_secs_f64();
    let x_of =
        |t: Time| rect.left() + (t.to_secs_f64() / secs).clamp(0.0, 1.0) as f32 * rect.width();
    if let Some(player) = &s.player {
        let gap = Time::seconds(Rational::new(1, 1000).unwrap_or(Rational::ONE));
        if let Some(spans) = player.cache().try_ranges(0, gap) {
            s.scrub_spans = spans;
        }
        for &(a, b) in &s.scrub_spans {
            let r =
                egui::Rect::from_x_y_ranges(x_of(a)..=x_of(b).max(x_of(a) + 1.0), rect.y_range());
            painter.rect_filled(r, 0.0, visuals.selection.bg_fill.gamma_multiply(0.5));
        }
    }
    let x = x_of(at);
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(2.0, visuals.strong_text_color()),
    );
    let last = last_frame_time(d, s.stream0());
    let time_at = |pos: egui::Pos2| {
        let f = f64::from(((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
        let t = Time::seconds(
            Rational::new((f * 1_000_000.0).round() as i64, 1_000_000).unwrap_or(Rational::ZERO),
        );
        let t = Time::seconds(
            t.as_rational()
                .checked_mul(d.as_rational())
                .unwrap_or(Rational::ZERO),
        );
        t.min(last)
    };
    let playing = s.snapshot.want_play;
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        s.scrub
            .move_to(s.player.as_ref(), time_at(pos), playing, now);
    }
    if response.drag_stopped() {
        s.scrub.release(s.player.as_ref(), now);
    } else if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        s.seek(time_at(pos), SeekMode::Exact);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_print_to_the_millisecond_rounded_down() {
        assert_eq!(format_time(Time::ZERO), "00:00:00.000");
        let t = Time::seconds(Rational::new(37_232_501, 10_000).unwrap());
        assert_eq!(format_time(t), "01:02:03.250");
        let t = Time::seconds(Rational::new(1001, 30_000).unwrap());
        assert_eq!(format_time(t), "00:00:00.033");
    }
}
