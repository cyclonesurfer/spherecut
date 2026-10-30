//! The viewer's paint callback (M1 plan section 7.3): the frame on
//! screen projected into a linear target of the viewer's physical size,
//! then encoded into egui's own render pass, one pixel per pixel.

use std::sync::Arc;

use egui::epaint::ViewportInPixels;
use egui_wgpu::{CallbackResources, CallbackTrait, ScreenDescriptor};
use glam::DVec2;
use spherecut_playback::SlotFrame;
use spherecut_render::{
    DebugMode, Encoder, Filter, Gpu, GpuFrame, LinearTarget, Projector, ViewParams,
};
use spherecut_sphere::pixel;

/// What a projection drew: the slot and the frame in it, through a view
/// and filter. A draw with the key of the last one, at the same size,
/// only encodes (section 7.3), so a 30 fps video on a 144 Hz display
/// projects 30 times a second. `prepare` compares keys itself, so a UI
/// pass that is never painted (egui discards some) can't lose a redraw.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawKey {
    pub slot: usize,
    pub frame: SlotFrame,
    pub view: ViewParams,
    pub filter: Filter,
}

/// What the callbacks share, kept in egui-wgpu's `callback_resources`:
/// inserted once, on the UI thread, before the first frame.
pub struct ViewerResources {
    gpu: Gpu,
    projector: Projector,
    encoder: Encoder,
    target: Option<LinearTarget>,
    /// What the target holds, if a projection: `paint` encodes it.
    last: Option<DrawKey>,
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
        }
        ViewerResources {
            gpu: gpu.clone(),
            projector,
            encoder: Encoder::new(gpu, target_format),
            target: None,
            last: None,
            projected: 0,
            encoded_only: 0,
        }
    }

    /// The linear target's size, for tests.
    pub fn target_extent(&self) -> Option<(u32, u32)> {
        self.target.as_ref().map(LinearTarget::extent)
    }

    /// What the target holds.
    pub fn last(&self) -> Option<&DrawKey> {
        self.last.as_ref()
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
}

impl std::fmt::Debug for ViewerCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerCallback")
            .field("rect", &self.rect)
            .field("shown", &self.shown.as_ref().map(|(_, k)| k))
            .finish()
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
        let Some(r) = res.get_mut::<ViewerResources>() else {
            return Vec::new();
        };
        // The rounding paint gets, from the surface's real size.
        let vp = ViewportInPixels::from_points(&self.rect, sd.pixels_per_point, sd.size_in_pixels);
        let size = physical(&vp);
        let mut resized = false;
        match &mut r.target {
            Some(t) if t.extent() == size => {}
            Some(t) => {
                if let Err(e) = t.resize(&r.gpu, size) {
                    tracing::warn!(error = %e, ?size, "the viewer's target can't be resized");
                    r.last = None;
                    return Vec::new();
                }
                resized = true;
            }
            None => match LinearTarget::new(&r.gpu, size) {
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
        if !resized && r.last.as_ref() == Some(key) {
            r.encoded_only += 1;
            return Vec::new();
        }
        let Some(target) = &r.target else {
            return Vec::new();
        };
        r.projector.prepare(key.filter, DebugMode::Off);
        match r.projector.record(
            enc,
            frame,
            &key.view,
            target,
            key.filter,
            DebugMode::Off,
            None,
        ) {
            Ok(()) => {
                r.last = Some(key.clone());
                r.projected += 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, "the viewer's projection failed");
                r.last = None;
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
    }
}
