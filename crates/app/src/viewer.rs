//! The viewer's paint callback (M1 plan section 7.3): the frame on
//! screen projected into a linear target of the viewer's physical size,
//! then encoded into egui's own render pass, one pixel per pixel. A
//! projection's time to record and its time on the GPU go to the
//! player's numbers (section 10), with the draws that projected and
//! those that only encoded.
//!
//! A source that isn't equirect, or has more than one video stream (a
//! camera's lenses, before M3 stitches them), is shown in the raw view
//! instead: each stream's picture flat, in a column of its own, as large
//! as its aspect fits, each in a target of its own size.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use egui::epaint::ViewportInPixels;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};
use glam::DVec2;
use spherecut_playback::{Counter, PerfStats, SlotFrame, Stage};
use spherecut_render::{
    DebugMode, Encoder, Filter, Gpu, GpuFrame, GpuTimers, LinearTarget, Projector, ViewParams,
};
use spherecut_sphere::pixel;

/// What a projection drew: the slot and the frame in it, through a view
/// and filter. A draw with the key of the last one, at the same size,
/// only encodes (section 7.3), so a 30 fps video on a 144 Hz display
/// projects 30 times a second. `prepare` compares keys itself, so a UI
/// pass that is never painted (egui discards some) can't lose a redraw;
/// and a projection counts as drawn only once `paint` ran after it, since
/// egui-wgpu records `prepare`'s commands before it acquires the
/// window's surface, and drops them when that fails.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawKey {
    pub slot: usize,
    pub frame: SlotFrame,
    pub view: ViewParams,
    pub filter: Filter,
}

/// What a raw view's column drew: the slot and the frame in it, with a
/// filter.
#[derive(Debug, Clone, PartialEq)]
pub struct RawKey {
    pub slot: usize,
    pub frame: SlotFrame,
    pub filter: Filter,
}

/// One stream's picture in the raw view, in its column.
pub struct RawDraw {
    pub column: usize,
    /// Held as [`ViewerCallback::shown`]'s is.
    pub frame: Arc<GpuFrame>,
    pub key: RawKey,
}

impl std::fmt::Debug for RawDraw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawDraw")
            .field("column", &self.column)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// What a raw view column shows: its frame, where in the viewer, and how
/// large, in pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct RawShown {
    pub key: RawKey,
    pub origin: [u32; 2],
    pub size: (u32, u32),
}

/// A raw view column's target, where it sits in the viewer, and what it
/// holds.
struct RawTarget {
    target: LinearTarget,
    origin: [u32; 2],
    last: Option<RawKey>,
}

/// Where column `column` of `columns` puts a `picture` in a viewer of
/// `size` pixels: the columns split the width, and the picture is as
/// large as its aspect fits in its column, centred. The origin, and the
/// size, at least 1 × 1.
pub fn raw_place(
    size: (u32, u32),
    columns: usize,
    column: usize,
    picture: (u32, u32),
) -> ([u32; 2], (u32, u32)) {
    let n = columns.max(1) as u64;
    let c = (column as u64).min(n - 1);
    let (w, h) = (u64::from(size.0.max(1)), u64::from(size.1.max(1)));
    let x0 = w * c / n;
    let cw = (w * (c + 1) / n - x0).max(1);
    let (pw, ph) = (u64::from(picture.0.max(1)), u64::from(picture.1.max(1)));
    let (tw, th) = if pw * h <= ph * cw {
        ((pw * h / ph).clamp(1, cw), h)
    } else {
        (cw, (ph * cw / pw).clamp(1, h))
    };
    let int = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    (
        [int(x0 + (cw - tw) / 2), int((h - th) / 2)],
        (int(tw), int(th)),
    )
}

/// What the callbacks share, kept in egui-wgpu's `callback_resources`:
/// inserted once, on the UI thread, before the first frame.
pub struct ViewerResources {
    gpu: Gpu,
    projector: Projector,
    encoder: Encoder,
    /// The projection pass's GPU time, when the device has timestamps.
    timers: Option<GpuTimers>,
    target: Option<LinearTarget>,
    /// What the target holds, if a projection: `paint` encodes it.
    last: Option<DrawKey>,
    /// The raw view's targets, by column.
    raw: Vec<Option<RawTarget>>,
    /// `paint` ran since `last` was recorded, so the projection was
    /// submitted with it. Set by `paint`, which has the resources only
    /// shared.
    painted: AtomicBool,
    /// Draws that projected, and draws that only encoded: for the tests.
    pub projected: u64,
    pub encoded_only: u64,
}

impl std::fmt::Debug for ViewerResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerResources")
            .field("format", &self.encoder.format())
            .field("target", &self.target.as_ref().map(LinearTarget::extent))
            .field("last", &self.last)
            .field("raw", &self.raw_last())
            .field("painted", &self.painted)
            .field("projected", &self.projected)
            .field("encoded_only", &self.encoded_only)
            .finish_non_exhaustive()
    }
}

impl ViewerResources {
    /// Pipelines for egui's `target_format`, built now so the first frame
    /// doesn't wait for them.
    pub fn new(gpu: &Gpu, target_format: wgpu::TextureFormat) -> ViewerResources {
        let projector = Projector::new(gpu);
        for filter in [Filter::Anisotropic, Filter::Isotropic] {
            projector.prepare(filter, DebugMode::Off);
            projector.prepare_flat(filter);
        }
        ViewerResources {
            gpu: gpu.clone(),
            projector,
            encoder: Encoder::new(gpu, target_format),
            timers: GpuTimers::new(gpu, "projection"),
            target: None,
            last: None,
            raw: Vec::new(),
            painted: AtomicBool::new(false),
            projected: 0,
            encoded_only: 0,
        }
    }

    /// The linear target's size, for tests.
    pub fn target_extent(&self) -> Option<(u32, u32)> {
        self.target.as_ref().map(LinearTarget::extent)
    }

    /// What the target holds, once a pass with it was painted.
    pub fn last(&self) -> Option<&DrawKey> {
        self.last
            .as_ref()
            .filter(|_| self.painted.load(Ordering::Relaxed))
    }

    /// What each raw view column holds, by column.
    pub fn raw_last(&self) -> Vec<Option<RawShown>> {
        self.raw
            .iter()
            .map(|t| {
                let t = t.as_ref()?;
                Some(RawShown {
                    key: t.last.clone()?,
                    origin: t.origin,
                    size: t.target.extent(),
                })
            })
            .collect()
    }
}

/// One frame's draw of the viewer.
pub struct ViewerCallback {
    /// The viewer's rect in points, as `ui()` laid it out.
    pub rect: egui::Rect,
    /// The slot to show and what to draw it with. Holding the slot's
    /// frame keeps the slot from being reused until egui has submitted
    /// this frame and dropped the callback.
    pub shown: Option<(Arc<GpuFrame>, DrawKey)>,
    /// The raw view's pictures, in `columns` columns; when there are
    /// any, `shown` isn't drawn.
    pub raw: Vec<RawDraw>,
    pub columns: usize,
    /// The player's numbers, which the draw adds to.
    pub perf: Option<Arc<PerfStats>>,
}

impl std::fmt::Debug for ViewerCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerCallback")
            .field("rect", &self.rect)
            .field("shown", &self.shown.as_ref().map(|(_, k)| k))
            .field(
                "raw",
                &self
                    .raw
                    .iter()
                    .map(|d| (d.column, &d.key))
                    .collect::<Vec<_>>(),
            )
            .field("columns", &self.columns)
            .finish_non_exhaustive()
    }
}

/// The viewer's size in physical pixels, at least 1 × 1, by the rounding
/// egui-wgpu gives the callback.
pub fn physical(vp: &ViewportInPixels) -> (u32, u32) {
    let w = u32::try_from(vp.width_px).unwrap_or(0).max(1);
    let h = u32::try_from(vp.height_px).unwrap_or(0).max(1);
    (w, h)
}

/// The viewer's rect in `area`: rounded inward to whole physical pixels
/// and one pixel in from each side, so that its edges fall on pixel
/// boundaries at any scale and no line egui draws beside it (the tiles'
/// gap, a panel's separator) spills its antialiasing onto the picture.
/// At 1.25 points a pixel, the tiles' gap line otherwise blends into the
/// picture's last column.
pub fn pixel_rect(area: egui::Rect, ppp: f32) -> egui::Rect {
    let left = (area.left() * ppp).ceil() + 1.0;
    let top = (area.top() * ppp).ceil() + 1.0;
    let right = ((area.right() * ppp).floor() - 1.0).max(left);
    let bottom = ((area.bottom() * ppp).floor() - 1.0).max(top);
    egui::Rect::from_min_max(
        egui::pos2(left / ppp, top / ppp),
        egui::pos2(right / ppp, bottom / ppp),
    )
}

/// `rect`'s pixels, rounded as `prepare` rounds them: the window's size
/// in pixels is its content rect in points times `pixels_per_point`,
/// rounded, as egui-wgpu's screen descriptor has it.
pub fn viewport(rect: egui::Rect, ctx: &egui::Context) -> ViewportInPixels {
    let ppp = ctx.pixels_per_point();
    let screen = ctx.content_rect().size() * ppp;
    ViewportInPixels::from_points(
        &rect,
        ppp,
        [screen.x.round() as u32, screen.y.round() as u32],
    )
}

/// A pointer position in points → the lens point under it, with the
/// rounding `prepare` gets: into physical pixels by `ppp`, less the
/// viewport's rounded origin, over its pixel size.
pub fn lens_point(pos: egui::Pos2, vp: &ViewportInPixels, ppp: f32) -> DVec2 {
    let x = f64::from(pos.x) * f64::from(ppp) - f64::from(vp.left_px);
    let y = f64::from(pos.y) * f64::from(ppp) - f64::from(vp.top_px);
    pixel::to_lens(physical(vp), x, y)
}

impl CallbackTrait for ViewerCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        sd: &ScreenDescriptor,
        enc: &mut wgpu::CommandEncoder,
        res: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let start = Instant::now();
        let Some(r) = res.get_mut::<ViewerResources>() else {
            return Vec::new();
        };
        // The rounding paint gets, from the surface's real size.
        let vp = ViewportInPixels::from_points(&self.rect, sd.pixels_per_point, sd.size_in_pixels);
        let size = physical(&vp);
        if !self.raw.is_empty() {
            self.prepare_raw(r, enc, size, start);
            return Vec::new();
        }
        r.raw.clear();
        let mut resized = false;
        match &mut r.target {
            Some(t) if t.extent() == size => {}
            Some(t) => {
                if let Err(e) = t.resize_unchecked(&r.gpu, size) {
                    tracing::warn!(error = %e, ?size, "the viewer's target can't be resized");
                    r.last = None;
                    return Vec::new();
                }
                resized = true;
            }
            None => match LinearTarget::new_unchecked(&r.gpu, size) {
                Ok(t) => {
                    r.target = Some(t);
                    resized = true;
                }
                Err(e) => {
                    tracing::warn!(error = %e, ?size, "the viewer's target can't be made");
                    return Vec::new();
                }
            },
        }
        let Some((frame, key)) = &self.shown else {
            r.last = None;
            return Vec::new();
        };
        if !resized && r.last.as_ref() == Some(key) && r.painted.load(Ordering::Relaxed) {
            r.encoded_only += 1;
            if let Some(perf) = &self.perf {
                perf.add(Counter::EncodedOnly, 1);
            }
            return Vec::new();
        }
        let Some(target) = &r.target else {
            return Vec::new();
        };
        // Both filters' pipelines were built with the resources. The
        // device's health is the upload thread's to check (D36).
        r.projector.record_unchecked(
            enc,
            frame,
            &key.view,
            target,
            key.filter,
            DebugMode::Off,
            r.timers.as_mut(),
        );
        r.last = Some(key.clone());
        r.painted.store(false, Ordering::Relaxed);
        r.projected += 1;
        if let Some(perf) = &self.perf {
            perf.record_since(Stage::RenderCpu, start);
            perf.add(Counter::Projected, 1);
            // The latest span read back, a frame or two old: the device is
            // polled by the upload thread and by every submit.
            if let Some(ms) = r.timers.as_mut().and_then(GpuTimers::poll) {
                perf.record(Stage::RenderGpu, ms);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        res: &CallbackResources,
    ) {
        let Some(r) = res.get::<ViewerResources>() else {
            return;
        };
        if !self.raw.is_empty() {
            paint_raw(r, &info, pass);
            r.painted.store(true, Ordering::Relaxed);
            return;
        }
        if self.shown.is_none() || r.last.is_none() {
            return;
        }
        let Some(target) = &r.target else {
            return;
        };
        let vp = info.viewport_in_pixels();
        let origin = [
            u32::try_from(vp.left_px).unwrap_or(0),
            u32::try_from(vp.top_px).unwrap_or(0),
        ];
        r.encoder.draw(pass, target, origin);
        r.painted.store(true, Ordering::Relaxed);
    }
}

impl ViewerCallback {
    /// The raw view's `prepare`: each column's target made or resized to
    /// its picture's place, and drawn flat when its frame changed or its
    /// last draw wasn't painted.
    fn prepare_raw(
        &self,
        r: &mut ViewerResources,
        enc: &mut wgpu::CommandEncoder,
        size: (u32, u32),
        start: Instant,
    ) {
        r.last = None;
        let columns = self.columns.max(1);
        r.raw.resize_with(columns, || None);
        r.raw.truncate(columns);
        let painted = r.painted.load(Ordering::Relaxed);
        let mut drawn = vec![false; columns];
        let mut recorded = false;
        for d in &self.raw {
            let Some(slot) = r.raw.get_mut(d.column) else {
                continue;
            };
            drawn[d.column] = true;
            let (origin, extent) = raw_place(size, columns, d.column, d.frame.shape().size);
            let t = match slot {
                Some(t) if t.target.extent() == extent => t,
                Some(t) => {
                    if let Err(e) = t.target.resize_unchecked(&r.gpu, extent) {
                        tracing::warn!(error = %e, ?extent, "a raw view target can't be resized");
                        *slot = None;
                        continue;
                    }
                    t.last = None;
                    t
                }
                None => match LinearTarget::new_unchecked(&r.gpu, extent) {
                    Ok(target) => slot.insert(RawTarget {
                        target,
                        origin,
                        last: None,
                    }),
                    Err(e) => {
                        tracing::warn!(error = %e, ?extent, "a raw view target can't be made");
                        continue;
                    }
                },
            };
            t.origin = origin;
            if painted && t.last.as_ref() == Some(&d.key) {
                continue;
            }
            r.projector.record_flat_unchecked(
                enc,
                &d.frame,
                &t.target,
                d.key.filter,
                r.timers.as_mut(),
            );
            t.last = Some(d.key.clone());
            recorded = true;
        }
        // A column with no picture this draw shows none.
        for (t, drawn) in r.raw.iter_mut().zip(drawn) {
            if !drawn && let Some(t) = t {
                t.last = None;
            }
        }
        if recorded {
            r.painted.store(false, Ordering::Relaxed);
            r.projected += 1;
        } else {
            r.encoded_only += 1;
        }
        if let Some(perf) = &self.perf {
            if recorded {
                perf.record_since(Stage::RenderCpu, start);
                perf.add(Counter::Projected, 1);
                if let Some(ms) = r.timers.as_mut().and_then(GpuTimers::poll) {
                    perf.record(Stage::RenderGpu, ms);
                }
            } else {
                perf.add(Counter::EncodedOnly, 1);
            }
        }
    }
}

/// The raw view's `paint`: each column's target at its place, scissored
/// to it, since the encode pass blacks out the rest of the viewport.
fn paint_raw(
    r: &ViewerResources,
    info: &egui::PaintCallbackInfo,
    pass: &mut wgpu::RenderPass<'static>,
) {
    let vp = info.viewport_in_pixels();
    let clip = info.clip_rect_in_pixels();
    for t in r.raw.iter().flatten() {
        if t.last.is_none() {
            continue;
        }
        let (w, h) = t.target.extent();
        let int = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        let x = vp.left_px.saturating_add(int(t.origin[0]));
        let y = vp.top_px.saturating_add(int(t.origin[1]));
        let x0 = x.max(clip.left_px).max(0);
        let y0 = y.max(clip.top_px).max(0);
        let x1 = x
            .saturating_add(int(w))
            .min(clip.left_px.saturating_add(clip.width_px));
        let y1 = y
            .saturating_add(int(h))
            .min(clip.top_px.saturating_add(clip.height_px));
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let px = |v: i32| u32::try_from(v).unwrap_or(0);
        pass.set_scissor_rect(px(x0), px(y0), px(x1 - x0), px(y1 - y0));
        r.encoder
            .draw(pass, &t.target, [px(x.max(0)), px(y.max(0))]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_raw_view_fits_each_picture_in_its_column() {
        // Two square lenses in a wide viewer: side by side, full height.
        assert_eq!(
            raw_place((1000, 400), 2, 0, (3840, 3840)),
            ([50, 0], (400, 400))
        );
        assert_eq!(
            raw_place((1000, 400), 2, 1, (3840, 3840)),
            ([550, 0], (400, 400))
        );
        // In a tall one: full column width, centred down.
        assert_eq!(
            raw_place((600, 900), 2, 1, (100, 100)),
            ([300, 300], (300, 300))
        );
        // One 2:1 picture in a 4:3 viewer: full width, letterboxed.
        assert_eq!(
            raw_place((800, 600), 1, 0, (2000, 1000)),
            ([0, 100], (800, 400))
        );
        // An odd width: the columns cover it, none wider than its share.
        let a = raw_place((1001, 1000), 2, 0, (1, 1));
        let b = raw_place((1001, 1000), 2, 1, (1, 1));
        assert_eq!((a, b), (([0, 250], (500, 500)), ([500, 249], (501, 501))));
        // Nothing smaller than a pixel, nothing out of range.
        assert_eq!(raw_place((0, 0), 0, 5, (0, 0)), ([0, 0], (1, 1)));
        let (o, s) = raw_place((7, 3), 3, 2, (1000, 1));
        assert!(o[0] + s.0 <= 7 && o[1] + s.1 <= 3, "{o:?} {s:?}");
    }
}
