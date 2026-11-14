//! odin's GFX11 R8 signature, as a still's slot check labels a slot that
//! holds other bytes than its frame's: copied from
//! `crates/playback/tests/tier2_playback.rs` at 4dd1e58 (`Plane`,
//! `split`, `yuv_of`, `read_planes`, `PlaneDiff`, `Cell`, `CELLS`,
//! `r8_cells` and `r8_block`; the playback check's words are its own),
//! cut to what one picture needs: no bands, no hash, and of the pass's
//! rules (read again the same, the cache right, no resync, shown once)
//! only the picture's, since a still has no pass. Kept a copy, not lifted
//! into testkit, which holds no GPU or media types; a change to the
//! playback check's signature is made here too.
//!
//! It labels a slot's line and never excuses one: a slot that isn't its
//! frame's fails the check either way.

use spherecut_core::video::{ColorFamily, VideoPlanes};
use spherecut_render::{Gpu, GpuFrame, readback, wgpu};

/// One 8-bit plane: its size in samples, and the samples, row after row.
#[derive(Clone, PartialEq, Eq)]
pub struct Plane {
    pub width: u32,
    pub height: u32,
    pub samples: Vec<u8>,
}

/// A picture's Y, U and V planes, interleaved chroma (NV12's) split, so
/// that a slot and a software decode compare plane for plane.
pub type Yuv = [Plane; 3];

/// A plane of `width` × `height` positions, `components` samples
/// interleaved at each, as a plane a component.
fn split(
    width: u32,
    height: u32,
    components: usize,
    samples: Vec<u8>,
) -> Result<Vec<Plane>, String> {
    match components {
        1 => Ok(vec![Plane {
            width,
            height,
            samples,
        }]),
        2 => {
            let (u, v): (Vec<u8>, Vec<u8>) = samples.chunks_exact(2).map(|c| (c[0], c[1])).unzip();
            Ok(vec![
                Plane {
                    width,
                    height,
                    samples: u,
                },
                Plane {
                    width,
                    height,
                    samples: v,
                },
            ])
        }
        n => Err(format!("a plane of {n} samples interleaved")),
    }
}

fn three(planes: Vec<Plane>) -> Result<Yuv, String> {
    let n = planes.len();
    <Yuv>::try_from(planes).map_err(|_| format!("{n} planes, not Y, U and V"))
}

/// `frame`'s planes, planar or semi-planar 8-bit YUV.
pub fn yuv_of(frame: &dyn VideoPlanes) -> Result<Yuv, String> {
    let layout = frame.layout();
    if layout.family != ColorFamily::Yuv || layout.bit_depth != 8 {
        return Err(format!("{} isn't 8-bit YUV", layout.name));
    }
    let mut planes = Vec::new();
    for (i, desc) in layout.planes.iter().enumerate() {
        let p = frame
            .plane(i)
            .ok_or_else(|| format!("{} has no plane {i} in memory", layout.name))?;
        let row = desc.row_bytes(p.width);
        let mut samples = Vec::with_capacity(row * p.height as usize);
        for y in 0..p.height {
            let bytes = p
                .row(y, row)
                .ok_or_else(|| format!("{}'s plane {i} has no row {y}", layout.name))?;
            samples.extend_from_slice(bytes);
        }
        planes.extend(split(
            p.width,
            p.height,
            usize::from(desc.components),
            samples,
        )?);
    }
    three(planes)
}

/// `slot`'s planes, level 0 (which the mips are made from), read back
/// from `gpu`, NV12's chroma split. Waits for the GPU.
pub fn read_planes(gpu: &Gpu, slot: &GpuFrame) -> Result<Yuv, String> {
    let mut planes = Vec::new();
    for (i, t) in (0..).map_while(|i| slot.plane_texture(i).map(|t| (i, t))) {
        let components = match t.format() {
            wgpu::TextureFormat::R8Unorm => 1,
            wgpu::TextureFormat::Rg8Unorm => 2,
            other => return Err(format!("plane {i} is {other:?}, not 8-bit")),
        };
        let texels = readback::read_texture(gpu, t, 0).map_err(|e| e.to_string())?;
        planes.extend(split(texels.width, texels.height, components, texels.data)?);
    }
    three(planes)
}

/// The most aligned 8×4 cells a [`PlaneDiff`] keeps, and that odin's
/// GFX11 R8 signature allows a frame: PR #32's wrong frames differed in
/// one cell or two, and damage in more is of another scale.
pub const CELLS: usize = 16;

/// An aligned 8×4 cell of a plane in which samples differ: its corner,
/// and its samples that differ as bits, bit `y%4 * 8 + x%8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub x: u32,
    pub y: u32,
    pub mask: u32,
}

/// Where one plane differs.
#[derive(Debug, Clone, Default)]
pub struct PlaneDiff {
    pub samples: usize,
    /// The largest difference of a sample.
    pub most: u8,
    /// x0, y0, x1, y1 of the samples that differ, inclusive.
    pub bounds: Option<[u32; 4]>,
    /// The aligned 8×4 cells it differs in, and the first [`CELLS`] of
    /// them in raster order, each with its samples that differ.
    pub cells: usize,
    pub first_cells: Vec<Cell>,
}

impl PlaneDiff {
    /// Where `got` differs from `want`, planes of one shape.
    pub fn of(got: &Plane, want: &Plane) -> PlaneDiff {
        let mut d = PlaneDiff::default();
        let width = (got.width as usize).max(1);
        // The cells of the row of cells under way, a mask each.
        let mut masks = vec![0u32; width.div_ceil(8)];
        let mut cell_row = 0;
        for (y, (g, w)) in (0u32..).zip(got.samples.chunks(width).zip(want.samples.chunks(width))) {
            if g == w {
                continue;
            }
            if y / 4 != cell_row {
                d.add_cells(&mut masks, cell_row);
                cell_row = y / 4;
            }
            for (x, (a, b)) in (0u32..).zip(g.iter().zip(w)) {
                if a == b {
                    continue;
                }
                d.samples += 1;
                d.most = d.most.max(a.abs_diff(*b));
                d.bounds = Some(match d.bounds {
                    None => [x, y, x, y],
                    Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                });
                masks[x as usize / 8] |= 1 << (y % 4 * 8 + x % 8);
            }
        }
        d.add_cells(&mut masks, cell_row);
        d
    }

    /// The cells of row of cells `row` that `masks` has samples in, a
    /// mask a cell of the row, counted and kept up to [`CELLS`]; `masks`
    /// left clear.
    fn add_cells(&mut self, masks: &mut [u32], row: u32) {
        for (x, mask) in (0u32..).zip(masks.iter_mut()) {
            if *mask == 0 {
                continue;
            }
            self.cells += 1;
            if self.first_cells.len() < CELLS {
                self.first_cells.push(Cell {
                    x: x * 8,
                    y: row * 4,
                    mask: *mask,
                });
            }
            *mask = 0;
        }
    }
}

/// Where Y's samples that differ, `y`, lie in their aligned 8×4 cells, as
/// the signature has it: each cell's block ([`r8_block`]), in at most
/// [`CELLS`] cells; or why they don't, each cell off it named.
pub fn r8_cells(y: &PlaneDiff) -> Result<String, String> {
    if y.cells == 0 {
        return Err("Y right".into());
    }
    if y.cells > CELLS {
        return Err(format!(
            "Y differs in {} aligned 8x4 cells, more than the {CELLS} the signature allows",
            y.cells
        ));
    }
    let (mut on, mut off) = (Vec::new(), Vec::new());
    for &c in &y.first_cells {
        match r8_block(c) {
            Ok(block) => on.push(block),
            Err(why) => off.push(why),
        }
    }
    if off.is_empty() {
        Ok(on.join(" and "))
    } else {
        Err(off.join(", "))
    }
}

/// Which of odin's GFX11 R8 blocks Y's samples that differ in `cell` are:
/// the 4×2 at x%8 4..7, y%4 2..3, the right 4×4 (x%8 4..7, y%4 0..3) or
/// the whole cell, their box the block's, corner to corner (a sample
/// inside it may be right, where the wrong byte happens to be the
/// truth's); or where in the cell they are instead.
pub fn r8_block(cell: Cell) -> Result<String, String> {
    let of = format!("of the 8x4 cell at x {} y {}", cell.x, cell.y);
    if cell.mask == 0 {
        return Err(format!("no sample differs {of}"));
    }
    let row = |r: u32| cell.mask >> (r * 8) & 0xff;
    let cols = (0..4).fold(0, |c, r| c | row(r));
    let rows = (0..4).fold(0u32, |b, r| if row(r) != 0 { b | 1 << r } else { b });
    let (c0, c1) = (cols.trailing_zeros(), 31 - cols.leading_zeros());
    let (r0, r1) = (rows.trailing_zeros(), 31 - rows.leading_zeros());
    let off = |why: &str| {
        Err(format!(
            "Y differs at x%8 {c0}..{c1}, y%4 {r0}..{r1} {of}: {why}"
        ))
    };
    match (c0, c1, r0, r1) {
        (4, 7, 2, 3) => Ok(format!("the 4x2 at x%8 4..7, y%4 2..3 {of}")),
        (4, 7, 0, 3) => Ok(format!("the right 4x4 (x%8 4..7) {of}")),
        (0, 7, 0, 3) => Ok(format!("the whole {of}")),
        _ if c0 >= 4 => off("its right half, but neither its 4x2 at y%4 2..3 nor its whole 4x4"),
        _ if c1 < 4 => off("its left half alone"),
        _ => off("across its halves, short of the whole cell"),
    }
}

/// Whether a slot read back, `got`, holds `want`'s samples, a software
/// decode's of the same frame: `Ok` when every plane is the same; else
/// each plane that differs, how much, and where `got` falls against the
/// signature's picture part (Y only, U and V right, each cell of Y's a
/// block, in at most [`CELLS`] cells): "on odin's GFX11 R8 signature"
/// with each cell's block, or "off it" with every way it is off.
pub fn slot_holds(got: &Yuv, want: &Yuv) -> Result<(), String> {
    let shape = |y: &Yuv| {
        y.iter()
            .map(|p| format!("{}x{}", p.width, p.height))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if shape(got) != shape(want) {
        return Err(format!(
            "planes of {}, where the frame's are {}",
            shape(got),
            shape(want)
        ));
    }
    if got == want {
        return Ok(());
    }
    let diffs: Vec<PlaneDiff> = got
        .iter()
        .zip(want)
        .map(|(g, w)| PlaneDiff::of(g, w))
        .collect();
    let mut said = Vec::new();
    for (name, d) in ["Y", "U", "V"].iter().zip(&diffs) {
        if let Some([x0, y0, x1, y1]) = d.bounds {
            said.push(format!(
                "{name}: {} samples differ, by at most {}, in {} 8x4 cells, x {x0}..{x1} y {y0}..{y1}",
                d.samples, d.most, d.cells
            ));
        }
    }
    let mut off = Vec::new();
    let on = match r8_cells(&diffs[0]) {
        Ok(blocks) => Some(blocks),
        Err(why) => {
            off.push(why);
            None
        }
    };
    for (name, d) in ["U", "V"].iter().zip(&diffs[1..]) {
        if d.bounds.is_some() {
            off.push(format!("{name} differs"));
        }
    }
    let label = match on {
        Some(blocks) if off.is_empty() => format!("on odin's GFX11 R8 signature: {blocks}"),
        _ => format!("off odin's GFX11 R8 signature: {}", off.join(", ")),
    };
    Err(format!("{}; {label}", said.join("; ")))
}
