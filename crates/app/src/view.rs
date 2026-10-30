//! What the viewer looks through: the orientation, the lens with its
//! projection morph, and the texture filter (M1 plan section 7.4).

use glam::{DQuat, DVec2};
use spherecut_render::Filter;
use spherecut_sphere::grab::{self, GrabFit, GrabStart};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection};

/// How long the projection switch takes.
pub const MORPH_SECONDS: f64 = 0.25;
/// The narrowest field of view zoom goes to.
pub const MIN_HFOV_DEG: f64 = 10.0;
/// Rectilinear's field of view is eased down to this at most when the
/// switch lands on it.
pub const RECT_MAX_HFOV_DEG: f64 = 170.0;
/// The tiny planet preset: stereographic at this field of view,
/// looking straight down.
pub const TINY_PLANET_HFOV_DEG: f64 = 300.0;
/// What scrolling a point does to the field of view: hfov ←
/// hfov · exp(−ZOOM_PER_POINT · scroll_y).
pub const ZOOM_PER_POINT: f64 = 0.0015;
/// The `+` and `−` keys' step.
pub const ZOOM_STEP: f64 = 1.15;

/// The three projections the viewer switches between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Preset {
    Rectilinear,
    Fisheye,
    TinyPlanet,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Rectilinear, Preset::Fisheye, Preset::TinyPlanet];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Rectilinear => "Rect",
            Preset::Fisheye => "Fisheye",
            Preset::TinyPlanet => "Tiny planet",
        }
    }

    pub fn projection(self) -> Projection {
        match self {
            Preset::Rectilinear => Projection::Rectilinear,
            Preset::Fisheye => Projection::Fisheye,
            Preset::TinyPlanet => Projection::Stereographic,
        }
    }
}

/// A projection's name, as the presets call it.
pub fn projection_label(p: Projection) -> &'static str {
    match p {
        Projection::Rectilinear => "Rect",
        Projection::Fisheye => "Fisheye",
        Projection::Stereographic => "Stereographic",
    }
}

/// A projection switch under way: the lens and orientation it started
/// from and goes to, eased with smoothstep over [`MORPH_SECONDS`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct Morph {
    start: f64,
    from: Lens,
    to_projection: Projection,
    hfov_to: f64,
    q_from: DQuat,
    q_to: DQuat,
}

/// The viewer's camera.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
    pub q: DQuat,
    pub lens: Lens,
    pub filter: Filter,
    pub preset: Preset,
    morph: Option<Morph>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            q: DQuat::IDENTITY,
            lens: Lens::new(Projection::Rectilinear, 100f64.to_radians()),
            filter: Filter::Anisotropic,
            preset: Preset::Rectilinear,
            morph: None,
        }
    }
}

fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl ViewState {
    /// Whether a projection switch is under way, so the viewer repaints.
    pub fn animating(&self) -> bool {
        self.morph.is_some()
    }

    /// Start switching to `preset` at time `now` (seconds, the UI's
    /// clock). The tiny planet looks straight down; the others keep the
    /// heading.
    pub fn switch_to(&mut self, preset: Preset, now: f64) {
        self.settle(now);
        let to = preset.projection();
        let ypr = Ypr::from_quat(self.q);
        let (hfov_to, q_to) = match preset {
            Preset::TinyPlanet => (
                TINY_PLANET_HFOV_DEG.to_radians(),
                Ypr::new(ypr.yaw, -std::f64::consts::FRAC_PI_2, 0.0).to_quat(),
            ),
            Preset::Rectilinear => {
                let pitch = if self.preset == Preset::TinyPlanet {
                    0.0
                } else {
                    ypr.pitch
                };
                (
                    self.lens.hfov.min(RECT_MAX_HFOV_DEG.to_radians()),
                    Ypr::new(ypr.yaw, pitch, ypr.roll).to_quat(),
                )
            }
            Preset::Fisheye => {
                let pitch = if self.preset == Preset::TinyPlanet {
                    0.0
                } else {
                    ypr.pitch
                };
                (
                    self.lens.hfov.max(180f64.to_radians()),
                    Ypr::new(ypr.yaw, pitch, ypr.roll).to_quat(),
                )
            }
        };
        self.preset = preset;
        self.morph = Some(Morph {
            start: now,
            from: self.lens,
            to_projection: to,
            hfov_to,
            q_from: self.q,
            q_to,
        });
    }

    /// Advance a switch to time `now`; at its end the lens is `to` alone.
    pub fn settle(&mut self, now: f64) {
        let Some(m) = self.morph else {
            return;
        };
        let t = smoothstep((now - m.start) / MORPH_SECONDS);
        if t >= 1.0 {
            self.lens = Lens::new(m.to_projection, m.hfov_to);
            self.q = m.q_to;
            self.morph = None;
            return;
        }
        // The morph blends from the lens the switch started on (itself
        // possibly a blend, which is then taken as its `from`).
        let from = if m.from.morph > 0.5 {
            m.from.to
        } else {
            m.from.from
        };
        self.lens = Lens {
            from,
            to: m.to_projection,
            morph: t,
            hfov: m.from.hfov + (m.hfov_to - m.from.hfov) * t,
        };
        self.q = spherecut_sphere::slerp(m.q_from, m.q_to, t);
    }

    /// The largest field of view the current lens allows.
    pub fn max_hfov(&self) -> f64 {
        self.lens.from.max_hfov().max(self.lens.to.max_hfov())
    }

    /// Zoom by `factor` (over 1 narrows) about lens point `p`, keeping
    /// the direction under it.
    pub fn zoom(&mut self, factor: f64, p: DVec2) {
        if self.morph.is_some() || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old = self.lens;
        let hfov = (old.hfov / factor).clamp(MIN_HFOV_DEG.to_radians(), self.max_hfov());
        if hfov == old.hfov {
            return;
        }
        let new = Lens { hfov, ..old };
        self.q = grab::zoom_about(self.q, &old, &new, p);
        self.lens = new;
    }

    /// Stop a projection switch where it is.
    pub fn cancel_morph(&mut self) {
        self.morph = None;
    }

    /// The View menu's morph slider: the lens between its two
    /// projections, as set.
    pub fn set_morph(&mut self, morph: f64) {
        self.morph = None;
        self.lens.morph = morph.clamp(0.0, 1.0);
        self.lens.hfov = self.lens.hfov.min(self.max_hfov());
    }

    /// The projection the morph slider goes to.
    pub fn set_morph_target(&mut self, to: Projection) {
        self.morph = None;
        self.lens.to = to;
        self.lens.hfov = self.lens.hfov.min(self.max_hfov());
    }

    /// Look forward, level.
    pub fn reset(&mut self) {
        self.morph = None;
        self.q = DQuat::IDENTITY;
        if self.preset == Preset::TinyPlanet {
            self.q = Ypr::new(0.0, -std::f64::consts::FRAC_PI_2, 0.0).to_quat();
        }
    }

    /// A drag's start at lens point `p`.
    pub fn grab(&self, p: DVec2) -> GrabStart {
        grab::start(self.q, &self.lens, p)
    }

    /// Move a drag to lens point `p`: level mode keeps the roll, free
    /// mode (Ctrl) takes the smallest rotation.
    pub fn drag_to(&mut self, start: &mut GrabStart, p: DVec2, free: bool) -> GrabFit {
        if free {
            self.q = grab::solve_free(self.q, start, &self.lens, p);
            return GrabFit::Exact;
        }
        let (q, fit) = grab::solve_level(start, &self.lens, p);
        let ypr = Ypr::from_quat(q);
        start.yaw = ypr.yaw;
        start.pitch = ypr.pitch;
        self.q = q;
        fit
    }

    pub fn ypr(&self) -> Ypr {
        Ypr::from_quat(self.q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_ends_on_its_projection_alone_and_eases_between() {
        let mut v = ViewState::default();
        v.switch_to(Preset::TinyPlanet, 10.0);
        assert!(v.animating());
        v.settle(10.0 + MORPH_SECONDS / 2.0);
        assert!((v.lens.morph - 0.5).abs() < 1e-12, "{:?}", v.lens);
        assert_eq!(v.lens.to, Projection::Stereographic);
        v.settle(10.0 + MORPH_SECONDS);
        assert!(!v.animating());
        assert_eq!(
            v.lens,
            Lens::new(Projection::Stereographic, TINY_PLANET_HFOV_DEG.to_radians())
        );
        assert!((v.ypr().pitch + std::f64::consts::FRAC_PI_2).abs() < 1e-9);
        // Back to rectilinear: level again, at most 170°.
        v.switch_to(Preset::Rectilinear, 20.0);
        v.settle(21.0);
        assert_eq!(v.lens.from, Projection::Rectilinear);
        assert!(v.lens.hfov <= RECT_MAX_HFOV_DEG.to_radians() + 1e-12);
        assert!(v.ypr().pitch.abs() < 1e-9);
    }

    #[test]
    fn zoom_stays_within_its_bounds() {
        let mut v = ViewState::default();
        for _ in 0..100 {
            v.zoom(2.0, DVec2::ZERO);
        }
        assert!((v.lens.hfov - MIN_HFOV_DEG.to_radians()).abs() < 1e-12);
        for _ in 0..100 {
            v.zoom(0.5, DVec2::ZERO);
        }
        assert!((v.lens.hfov - Projection::Rectilinear.max_hfov()).abs() < 1e-12);
    }
}
