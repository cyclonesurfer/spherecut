//! `look` on synthetic media (a1-plan section 11.2), rendered on this
//! machine's GPU or lavapipe: tiles in the request's order at the times
//! they say, the views' and cube faces' markers where D33 puts them, the
//! sphere's grid on the lines it labels, raw tiles of a file that isn't
//! equirect, sheets, labels in their bands, the image budget, `output`, and
//! every limit at the argument it is about.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use spherecut_agent::error::codes;
use spherecut_agent::limits::{JPEG_QSCALE, JPEG_QSCALE_MAX, MAX_IMAGE_BUDGET, MIN_TILE_WIDTH};
use spherecut_agent::rpc::{CallCx, Image, Tools};
use spherecut_agent::still::{self, Options, PairOptions, Still};
use spherecut_agent::tools::Spherecut;
use spherecut_agent::{Roots, ToolError};
use spherecut_core::TimeSpec;
use spherecut_core::cancel::StopSource;
use spherecut_media::Source;
use spherecut_media::backend::SeekMode;
use spherecut_render::compare::sample_diff;
use spherecut_render::{AdapterChoice, Depth, Filter, OutKind, Pixels, ViewParams};
use spherecut_sphere::orient::Ypr;
use spherecut_sphere::perspective::{Lens, Projection as LensProjection};
use spherecut_synth::insv::{InsvFault, InsvSpec, write_insv};
use spherecut_synth::lens::{compose, x5_like, x5_like_planted};
use spherecut_synth::media::{self, Codec, Spec};
use spherecut_synth::{AXIS_MARKERS, RgbImage, decode_frame_code};
use spherecut_testkit::control::must_fail;

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("look-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    /// The tools, reading under this directory.
    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), None).unwrap())
    }

    /// The same, with room inline for a PNG.
    fn roomy(&self) -> Spherecut {
        self.tools().image_budget(MAX_IMAGE_BUDGET)
    }

    /// The tools, reading and writing under this directory.
    fn writing(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.0), Some(&self.0)).unwrap())
    }
}

/// synth's grid, 30 fps, `frames` long, lossless so that every pixel is
/// the pattern's; with Spherical Video V2 metadata unless `plain`.
fn grid(dir: &Path, name: &str, size: (u32, u32), frames: u32, plain: bool) -> PathBuf {
    let spec = Spec {
        name: name.into(),
        width: size.0,
        height: size.1,
        frames,
        codec: Codec::H264Lossless,
        spherical: !plain,
        ..Spec::default()
    };
    media::write(&spec, dir).unwrap()
}

fn look(tools: &Spherecut, args: Value) -> Result<(Value, Vec<Image>), ToolError> {
    let cx = CallCx::standalone(Instant::now() + Duration::from_secs(50));
    tools.call("look", args, &cx).map(|o| (o.result, o.images))
}

fn failed(tools: &Spherecut, args: Value) -> ToolError {
    match look(tools, args.clone()) {
        Ok((r, _)) => panic!("{args} gave {r}"),
        Err(e) => e,
    }
}

/// The one image, a PNG, decoded.
fn png(images: &[Image]) -> image::RgbImage {
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].mime, "image/png");
    image::load_from_memory(&images[0].data).unwrap().to_rgb8()
}

fn px(img: &image::RgbImage, x: u32, y: u32) -> [f64; 3] {
    let p = img.get_pixel(x, y).0;
    [0, 1, 2].map(|c| f64::from(p[c]) / 255.0)
}

fn far(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|c| sample_diff(a[c], b[c])).fold(0.0, f64::max)
}

/// Tile `t`'s picture: `[x, y, w, h]`.
fn rect(t: &Value) -> [u32; 4] {
    let r: Vec<u32> = t["rect"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    [r[0], r[1], r[2], r[3]]
}

fn centre(img: &image::RgbImage, t: &Value) -> [f64; 3] {
    let [x, y, w, h] = rect(t);
    px(img, x + w / 2, y + h / 2)
}

/// Tile `t`'s picture as synth's image, for its frame code.
fn crop(img: &image::RgbImage, t: &Value) -> RgbImage {
    let [x, y, w, h] = rect(t);
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for yy in y..y + h {
        for xx in x..x + w {
            pixels.push(px(img, xx, yy));
        }
    }
    RgbImage {
        width: w,
        height: h,
        pixels,
    }
}

/// Each tile's label band, above its picture, holds text: ink and the
/// band's colour, nothing else.
fn labelled(img: &image::RgbImage, tiles: &[Value]) {
    const INK: [u8; 3] = [240, 240, 240];
    const BAND: [u8; 3] = [16, 16, 16];
    for t in tiles {
        let [x, y, w, _] = rect(t);
        let band = if w >= 320 { 44 } else { 22 };
        let mut inked = 0;
        for yy in y - band..y {
            for xx in x..x + w {
                let p = img.get_pixel(xx, yy).0;
                assert!(p == INK || p == BAND, "{p:?} at {xx},{yy} in {t}");
                inked += usize::from(p == INK);
            }
        }
        assert!(inked > 20, "no label over {t}");
    }
}

fn marker(i: usize) -> [f64; 3] {
    AXIS_MARKERS[i].1
}

#[test]
fn views_of_a_time_are_tiles_in_order_with_their_markers_centred() {
    let dir = TempDir::new("views");
    let clip = grid(&dir.0, "grid", (1024, 512), 30, false);
    let views = [0.0, 90.0, 180.0, -90.0];
    let (r, images) = look(
        &dir.roomy(),
        json!({"file": clip, "times": ["10f"], "format": "png", "size": 1024,
               "views": views.iter().map(|y| json!({"yaw": y})).collect::<Vec<_>>()}),
    )
    .unwrap();
    let img = png(&images);
    let tiles = r["tiles"].as_array().unwrap();
    assert_eq!(tiles.len(), 4, "{r}");
    assert_eq!(r["image"]["width"], img.width());
    assert_eq!(r["image"]["height"], img.height());
    assert_eq!(r["image"]["format"], "png");
    assert_eq!(r["image"]["bytes"], images[0].data.len());
    for (i, t) in tiles.iter().enumerate() {
        assert_eq!(t["index"], i);
        assert_eq!(t["time"]["frame"], 10, "{t}");
        assert_eq!(t["requested"]["frame"], 10, "{t}");
        assert_eq!(t["view"]["yaw"], views[i], "{t}");
        assert_eq!(t["view"]["fov"], 90.0);
        assert_eq!(t["view"]["projection"], "rectilinear");
        assert!(
            (t["view"]["vfov"].as_f64().unwrap() - 58.716).abs() < 0.01,
            "{t}"
        );
        assert!(t["decode"]["path"].is_string(), "{t}");
        // F, R, B, L: the markers ahead, right, behind and left (D33).
        let c = centre(&img, t);
        assert!(
            far(c, marker(i)) <= 0.1,
            "tile {i}: {c:?}, not {:?}",
            marker(i)
        );
    }
    labelled(&img, tiles);
    assert_eq!(r["warnings"], json!([]));
}

#[test]
fn cube_faces_look_along_the_six_axes() {
    let dir = TempDir::new("cube");
    let clip = grid(&dir.0, "grid", (1024, 512), 2, false);
    let (r, images) = look(
        &dir.roomy(),
        json!({"file": clip, "mode": "cube", "format": "png"}),
    )
    .unwrap();
    let img = png(&images);
    let tiles = r["tiles"].as_array().unwrap();
    let faces: Vec<&str> = tiles.iter().map(|t| t["face"].as_str().unwrap()).collect();
    assert_eq!(faces, ["front", "right", "back", "left", "up", "down"]);
    for (i, t) in tiles.iter().take(4).enumerate() {
        let [_, _, w, h] = rect(t);
        assert_eq!(w, h, "a face is square: {t}");
        let c = centre(&img, t);
        assert!(far(c, marker(i)) <= 0.1, "{}: {c:?}", faces[i]);
    }
    // The poles filter to neutral greys (see frame's Euler test), the up
    // one the brighter.
    let (up, down) = (centre(&img, &tiles[4]), centre(&img, &tiles[5]));
    assert!(up[0] > down[0] + 0.2, "up {up:?}, down {down:?}");
    labelled(&img, tiles);
}

#[test]
fn the_sphere_shows_the_frame_asked_for_under_a_grid_on_its_lines() {
    let dir = TempDir::new("sphere");
    let clip = grid(&dir.0, "grid", (1024, 512), 30, false);
    let tools = dir.roomy();
    let (r, images) = look(
        &tools,
        json!({"file": clip, "mode": "sphere", "times": ["20f", "7f"], "format": "png",
               "size": 1568}),
    )
    .unwrap();
    let img = png(&images);
    let tiles = r["tiles"].as_array().unwrap();
    // In the request's order, though decoded in the file's.
    for (t, want) in tiles.iter().zip([20, 7]) {
        assert_eq!(t["time"]["frame"], want);
        assert_eq!(decode_frame_code(&crop(&img, t)), Some(want), "{t}");
        assert!(t.get("view").is_none());
    }

    // The same frame raw is the picture without the grid, laid out the
    // same: the difference is the grid and its labels alone.
    let one = |mode: &str| {
        let (r, images) = look(
            &tools,
            json!({"file": clip, "mode": mode, "times": ["7f"], "format": "png"}),
        )
        .unwrap();
        (r["tiles"][0].clone(), png(&images))
    };
    let (t, sphere) = one("sphere");
    let (t_raw, raw) = one("raw");
    assert_eq!(rect(&t), rect(&t_raw));
    let [x, y, w, h] = rect(&t);
    let col = |yaw: f64| x + ((yaw + 180.0) / 360.0 * f64::from(w)).round() as u32;
    let row = |pitch: f64| y + ((90.0 - pitch) / 180.0 * f64::from(h)).round() as u32;
    let lighter = |xx: u32, yy: u32| {
        let (a, b) = (px(&sphere, xx, yy), px(&raw, xx, yy));
        (0..3).map(|c| a[c] - b[c]).sum::<f64>() / 3.0
    };
    // Down each yaw line, away from the labels: lighter all the way.
    for yaw in [-150.0, -90.0, -60.0, 0.0, 30.0, 120.0, 150.0] {
        let x = col(yaw);
        for yy in (y + 40..y + h - 10).step_by(7) {
            if yy.abs_diff(row(0.0)) < 2 {
                continue;
            }
            assert!(
                lighter(x, yy) > 0.0 || px(&raw, x, yy)[0] > 0.93,
                "yaw {yaw} at {yy}"
            );
        }
        // Halfway between lines, on the grid clip's own 15° lines, ours
        // are not: nothing changed there.
        let between = col(yaw + 15.0);
        assert_eq!(lighter(between, row(-45.0)), 0.0, "yaw {}", yaw + 15.0);
    }
    for pitch in [-60.0, -30.0, 30.0, 60.0] {
        let yy = row(pitch);
        assert!(lighter(col(-100.0), yy) > 0.0, "pitch {pitch}");
        assert_eq!(lighter(col(-100.0), row(pitch + 15.0)), 0.0);
    }
    // The clip's bright prime meridian is our yaw 0.
    let brightest = (x + w / 2 - 6..x + w / 2 + 6)
        .max_by(|&a, &b| {
            let l = |xx| px(&raw, xx, row(-45.0)).iter().sum::<f64>();
            l(a).total_cmp(&l(b))
        })
        .unwrap();
    assert!(
        brightest.abs_diff(col(0.0)) <= 1,
        "{brightest} vs {}",
        col(0.0)
    );
}

#[test]
fn a_file_that_isnt_equirect_is_seen_raw() {
    let dir = TempDir::new("raw");
    let clip = grid(&dir.0, "plain", (512, 256), 10, true);
    let tools = dir.roomy();
    let e = failed(&tools, json!({"file": clip}));
    assert_eq!(e.code, codes::NOT_EQUIRECT, "{e:?}");
    assert_eq!(e.argument.as_deref(), Some("file"));
    let next = e.next.unwrap_or_default();
    assert!(
        next.contains("\"raw\"") && next.contains("assume_equirect"),
        "{next}"
    );
    assert!(
        !e.message.contains("plain"),
        "no file name in an error: {}",
        e.message
    );

    let (r, images) = look(
        &tools,
        json!({"file": clip, "mode": "raw", "times": ["4f"], "format": "png"}),
    )
    .unwrap();
    let t = &r["tiles"][0];
    assert_eq!(t["stream"], 0, "{r}");
    let img = png(&images);
    assert_eq!(decode_frame_code(&crop(&img, t)), Some(4));
    // A 2:1 picture fills a 2:1 tile.
    let [_, _, w, h] = rect(t);
    assert_eq!(w, 2 * h);

    let (r, _) = look(
        &tools,
        json!({"file": clip, "assume_equirect": true, "times": ["4f"]}),
    )
    .unwrap();
    assert_eq!(r["tiles"][0]["time"]["frame"], 4);
}

// A camera's pair of lenses (M3.7).

/// The name of a run's folder of `.insv` files, before its process's id.
const RUN: &str = "look-insv-";

/// The `.insv` files the pair's tests look at, in a folder of this
/// binary's kept for the run.
struct Insv {
    dir: PathBuf,
    /// `InsvSpec::small()`'s at 2 frames: two 384² lenses whose
    /// calibration reads.
    small: PathBuf,
    /// Two 128² lenses, 2 frames, with no calibration
    /// (`InsvFault::NoCalibration`).
    uncalibrated: PathBuf,
}

impl Insv {
    /// The tools, reading under the folder, with room inline for a PNG.
    fn tools(&self) -> Spherecut {
        Spherecut::new(Roots::new(std::slice::from_ref(&self.dir), None).unwrap())
            .image_budget(MAX_IMAGE_BUDGET)
    }
}

/// Both files, written once a binary, side by side, into a folder whose
/// name ends in this process's id; the folders of runs that have ended go
/// first, so that they don't pile up.
fn insv() -> &'static Insv {
    static FILES: OnceLock<Insv> = OnceLock::new();
    FILES.get_or_init(|| {
        let began = Instant::now();
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
        sweep(root);
        let dir = root.join(format!("{RUN}{}", std::process::id()));
        let specs = [
            InsvSpec {
                frames: 2,
                ..InsvSpec::small()
            },
            InsvSpec {
                frames: 2,
                fault: Some(InsvFault::NoCalibration),
                rig: compose(&x5_like(), &x5_like_planted(), 0, [128; 2]),
                ..InsvSpec::small()
            },
        ];
        let [small, uncalibrated] = std::thread::scope(|s| {
            let dir = &dir;
            specs
                .each_ref()
                .map(|spec| {
                    let folder = dir.join(if spec.fault.is_some() {
                        "none"
                    } else {
                        "small"
                    });
                    s.spawn(move || write_insv(spec, &folder).unwrap().master)
                })
                .map(|h| h.join().unwrap())
        });
        eprintln!(
            "the .insv files written in {:.3} s",
            began.elapsed().as_secs_f64()
        );
        Insv {
            dir,
            small,
            uncalibrated,
        }
    })
}

/// Removes each folder in `root` named [`RUN`] and the id of a process
/// that is no longer running.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let pid = name
            .to_str()
            .and_then(|n| n.strip_prefix(RUN))
            .and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid
            && !Path::new(&format!("/proc/{pid}")).exists()
        {
            std::fs::remove_dir_all(e.path()).ok();
        }
    }
}

/// A look at a file and its first tile: the tile's picture, its rect and
/// the result.
type Looked = (Vec<u8>, [u32; 4], Value);

/// look at `file` as PNG at frame 0, with `extra`'s arguments.
fn look_at(file: &Path, extra: Value) -> Looked {
    let mut args = json!({"file": file, "format": "png", "times": ["0f"]});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let (r, images) = look(&insv().tools(), args).unwrap();
    let t = r["tiles"][0].clone();
    (picture_of(&png(&images), &t), rect(&t), r)
}

/// look at `InsvSpec::small()`'s file in mode view at +X (yaw 90, the
/// seam's side), 256 pixels wide, once a binary: the stitch test's check
/// and the uncalibrated test's control.
fn small_at_x() -> &'static Looked {
    static LOOKED: OnceLock<Looked> = OnceLock::new();
    LOOKED.get_or_init(|| look_at(&insv().small, json!({"view": {"yaw": 90}, "size": 256})))
}

/// Tile `t`'s picture, its pixels' bytes row by row.
fn picture_of(img: &image::RgbImage, t: &Value) -> Vec<u8> {
    let [x, y, w, h] = rect(t);
    (y..y + h)
        .flat_map(|yy| (x..x + w).flat_map(move |xx| img.get_pixel(xx, yy).0))
        .collect()
}

/// An 8-bit render's bytes.
fn bytes(img: Image8) -> Vec<u8> {
    match img.pixels {
        Pixels::Rgb8(p) => p,
        other => panic!("not 8-bit: {other:?}"),
    }
}

type Image8 = spherecut_render::Image;

/// `a` and `b` byte for byte, or how many bytes differ.
fn same(a: &[u8], b: &[u8]) -> Result<(), String> {
    let differ = a.iter().zip(b).filter(|(x, y)| x != y).count();
    if a.len() == b.len() && differ == 0 {
        Ok(())
    } else {
        Err(format!(
            "{differ} of {} bytes differ ({} beside {})",
            a.len(),
            a.len(),
            b.len()
        ))
    }
}

/// A pixel no lens covered is black: none of its channels above this.
const DARK: u8 = 8;

/// That every pixel lit in `truth` is lit in `seen`, both `w` wide.
fn lit_where(truth: &[u8], seen: &[u8], w: u32) -> Result<(), String> {
    let lit = |p: &[u8]| p.iter().any(|&c| c > DARK);
    let dark: Vec<(usize, usize)> = truth
        .chunks(3)
        .zip(seen.chunks(3))
        .enumerate()
        .filter(|(_, (t, s))| lit(t) && !lit(s))
        .map(|(i, _)| (i % w as usize, i / w as usize))
        .collect();
    match dark.first() {
        None => Ok(()),
        Some(at) => Err(format!("{} pixels dark, the first at {at:?}", dark.len())),
    }
}

/// `src`, `sw` × `sh`, scaled to `w` × `h` by area: each pixel the mean
/// of the source's over its footprint, each source pixel weighed by how
/// much of it lies there.
fn by_area(src: &[u8], (sw, sh): (u32, u32), (w, h): (u32, u32)) -> Vec<u8> {
    let (fx, fy) = (f64::from(sw) / f64::from(w), f64::from(sh) / f64::from(h));
    // The source pixels a span covers, with how much of each.
    let span = |i: u32, f: f64, n: u32| -> Vec<(u32, f64)> {
        let (a, b) = (f64::from(i) * f, f64::from(i + 1) * f);
        (a.floor() as u32..(b.ceil() as u32).min(n))
            .map(|k| (k, (b.min(f64::from(k + 1)) - a.max(f64::from(k))).max(0.0)))
            .collect()
    };
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        let rows = span(y, fy, sh);
        for x in 0..w {
            let cols = span(x, fx, sw);
            let mut sum = [0.0; 3];
            let mut weight = 0.0;
            for &(yy, wy) in &rows {
                for &(xx, wx) in &cols {
                    let i = ((yy * sw + xx) * 3) as usize;
                    for c in 0..3 {
                        sum[c] += f64::from(src[i + c]) * wx * wy;
                    }
                    weight += wx * wy;
                }
            }
            out.extend(sum.map(|v| (v / weight).round() as u8));
        }
    }
    out
}

/// look's view at `yaw`, its default fov of 90° across.
fn view_at(yaw: f64) -> ViewParams {
    ViewParams::new(
        Ypr::from_degrees(yaw, 0.0, 0.0).to_quat(),
        Lens::new(LensProjection::Rectilinear, 90f64.to_radians()),
    )
}

/// The whole sphere as equirect, yaw 0 at the centre.
fn equirect() -> ViewParams {
    ViewParams {
        out: OutKind::Equirect,
        ..view_at(0.0)
    }
}

/// How look renders a tile.
const LOOK: still::Look = still::Look {
    draw: still::Draw::View,
    filter: Filter::Isotropic,
    depth: Depth::Eight,
};

/// `look` at a calibrated `.insv` stitches its lens pair on the GPU,
/// in this process as `Still::open_pair` and `StillPair::render` do:
/// mode view at +X (yaw 90, the seam's side) is byte for byte their
/// render at the tile's size; and a sphere tile under 160 pixels wide is
/// rendered at 160 or more and scaled down (amendment 5), so that every
/// pixel lit in the 160-pixel stitch scaled down by area is lit in it.
/// Controls: assume_equirect's tile, the file read as one equirect,
/// differs; and the stitch rendered at the tile's own width leaves
/// pixels at the seam that no lens covers.
#[test]
fn look_stitches_an_insv() {
    let began = Instant::now();
    let files = insv();
    let gpu = still::gpu(
        &AdapterChoice::from_env().unwrap_or(AdapterChoice::Auto),
        &Options::default(),
    )
    .unwrap();
    let source = StopSource::new();
    let stop = still::Stop::of(&source);
    let src = Source::path(&files.small);
    let report = still::probe_report(src, &stop).unwrap();
    let mut pair = Still::open_pair(
        src,
        &report,
        Some(&gpu),
        &Options::default(),
        &PairOptions::default(),
        &stop,
        &mut Vec::new(),
    )
    .unwrap();
    let d = pair
        .seek(TimeSpec::Frame(0), SeekMode::Exact, &mut Vec::new())
        .unwrap();

    let ask = |extra: Value| look_at(&files.small, extra);
    let (tile, [_, _, w, h], r) = small_at_x();
    assert_eq!(r["tiles"][0]["view"]["yaw"], 90.0, "{r}");
    let stitched = bytes(pair.render(&d, &[view_at(90.0)], (*w, *h), LOOK).unwrap());
    same(tile, &stitched).unwrap();
    let (flat, ..) = ask(json!({"view": {"yaw": 90}, "size": 256, "assume_equirect": true}));
    must_fail("assume_equirect's tile", same(&flat, &stitched));

    let (tile, [_, _, w, h], r) = ask(json!({"mode": "sphere", "size": 128}));
    assert!(
        (MIN_TILE_WIDTH..160).contains(&w),
        "a sphere tile {w} wide: {r}"
    );
    let wide = bytes(pair.render(&d, &[equirect()], (160, 80), LOOK).unwrap());
    let truth = by_area(&wide, (160, 80), (w, h));
    lit_where(&truth, &tile, w).unwrap();
    let own = bytes(pair.render(&d, &[equirect()], (w, h), LOOK).unwrap());
    must_fail(
        "the stitch at the tile's own width",
        lit_where(&truth, &own, w),
    );
    eprintln!(
        "look_stitches_an_insv: {:.3} s",
        began.elapsed().as_secs_f64()
    );
}

/// That `r` shows a file as its two lenses, raw: a tile for each of its
/// two video streams, as coded, and the warning that says why.
fn shown_as_lenses(r: &Value) -> Result<(), String> {
    shown_as_lenses_at(r, 1)
}

/// `shown_as_lenses` at each of `times` times.
fn shown_as_lenses_at(r: &Value, times: usize) -> Result<(), String> {
    let streams: Vec<&Value> = r["tiles"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| &t["stream"])
        .collect();
    let lenses = [json!(0), json!(1)];
    let expected: Vec<&Value> = lenses.iter().cycle().take(2 * times).collect();
    let warned = r["warnings"].as_array().is_some_and(|w| {
        w.iter().any(|w| {
            w["code"] == "no_calibration"
                && w["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("no calibration: shown as its lenses"))
        })
    });
    if streams == expected && warned {
        Ok(())
    } else {
        Err(format!("streams {streams:?}, warned {warned}: {r}"))
    }
}

/// `look` at an `.insv` with no calibration, in mode view, shows its two
/// lenses' tracks side by side as mode raw shows them, with the
/// `no_calibration` warning "no calibration: shown as its lenses".
/// Control: `InsvSpec::small()`'s file, whose calibration reads, isn't
/// shown so in mode view (the stitch test's look at +X).
#[test]
fn look_shows_an_uncalibrated_insv_as_lenses() {
    let began = Instant::now();
    let tools = insv().tools();
    let at = |extra: Value| {
        let mut args =
            json!({"file": insv().uncalibrated, "format": "png", "size": 256, "times": ["1f"]});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        look(&tools, args).unwrap()
    };
    let (r, images) = at(json!({}));
    shown_as_lenses(&r).unwrap();
    let (raw, raw_images) = at(json!({"mode": "raw"}));
    assert_eq!(r["tiles"], raw["tiles"], "{r}\n{raw}");
    assert!(png(&images) == png(&raw_images), "not raw's picture");
    must_fail("small()'s file", shown_as_lenses(&small_at_x().2));
    eprintln!(
        "look_shows_an_uncalibrated_insv_as_lenses: {:.3} s",
        began.elapsed().as_secs_f64()
    );
}

/// That `got` refuses a look as more tiles than an image holds because
/// the file is shown as its two lenses, blamed on `arg`: `out_of_range`,
/// the 26 tiles and the lenses named, and half `MAX_TILES` times as the
/// limit.
fn refused_as_lenses(got: Result<(Value, Vec<Image>), ToolError>, arg: &str) -> Result<(), String> {
    let e = match got {
        Ok((r, _)) => {
            return Err(format!(
                "{} tiles shown",
                r["tiles"].as_array().map_or(0, Vec::len)
            ));
        }
        Err(e) => e,
    };
    let valid = e.valid.as_deref().unwrap_or("");
    if e.code == codes::OUT_OF_RANGE
        && e.argument.as_deref() == Some(arg)
        && e.message.contains("26 tiles")
        && e.message.contains("its 2 lenses")
        && valid.starts_with("12 times at most")
    {
        Ok(())
    } else {
        Err(format!("{e:?}"))
    }
}

/// A look at an `.insv` with no calibration counts the tiles its lenses
/// draw, two a time, whatever mode was asked (the M3.7 review's N10): a
/// sheet of 13 times, which the plan takes as 13 tiles, is refused as 26
/// at `times`, and 13 spread by `count` at `count`, each saying the file
/// shows as its 2 lenses, 12 times at most. Control: 12 times, 24 tiles,
/// are shown.
#[test]
fn an_uncalibrated_insvs_lenses_count_as_its_tiles() {
    let began = Instant::now();
    let tools = insv().tools();
    let times = |n: usize| -> Vec<&str> { ["0f", "1f"].into_iter().cycle().take(n).collect() };
    let sheet = |extra: Value| {
        let mut args = json!({"file": insv().uncalibrated, "mode": "sheet", "size": 1568});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        look(&tools, args)
    };
    refused_as_lenses(sheet(json!({"times": times(13)})), "times").unwrap();
    refused_as_lenses(sheet(json!({"count": 13})), "count").unwrap();
    let twelve = sheet(json!({"times": times(12)}));
    let (r, _) = twelve.as_ref().expect("12 times are shown");
    shown_as_lenses_at(r, 12).unwrap();
    must_fail("12 times", refused_as_lenses(twelve, "times"));
    eprintln!(
        "an_uncalibrated_insvs_lenses_count_as_its_tiles: {:.3} s",
        began.elapsed().as_secs_f64()
    );
}

#[test]
fn a_sheet_spreads_its_times_over_the_clip() {
    let dir = TempDir::new("sheet");
    let clip = grid(&dir.0, "grid", (512, 256), 30, false);
    let tools = dir.tools();
    let (r, images) = look(&tools, json!({"file": clip, "mode": "sheet"})).unwrap();
    let frames: Vec<u64> = r["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["time"]["frame"].as_u64().unwrap())
        .collect();
    assert_eq!(frames.len(), 12);
    assert_eq!((frames[0], frames[11]), (0, 29), "{frames:?}");
    assert!(frames.windows(2).all(|w| w[0] < w[1]), "{frames:?}");
    assert_eq!(images[0].mime, "image/jpeg");
    let q = r["image"]["quality"].as_u64().unwrap();
    assert!(
        (u64::from(JPEG_QSCALE)..=u64::from(JPEG_QSCALE_MAX)).contains(&q),
        "{r}"
    );

    // count between two times, both included.
    let (r, _) = look(
        &tools,
        json!({"file": clip, "times": ["0", "0.5"], "count": 3}),
    )
    .unwrap();
    let got: Vec<(&str, u64)> = r["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["requested"]["exact"].as_str().unwrap(),
                t["time"]["frame"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(got, [("0", 0), ("1/4", 7), ("1/2", 15)]);

    // A sheet with a view shows it at each time.
    let (r, _) = look(
        &tools,
        json!({"file": clip, "mode": "sheet", "count": 4, "view": {"yaw": 90, "fov": 60}}),
    )
    .unwrap();
    for t in r["tiles"].as_array().unwrap() {
        assert_eq!(t["view"]["yaw"], 90.0, "{t}");
    }
}

/// A JPEG's size, from its SOF0 or SOF2 segment.
fn jpeg_size(data: &[u8]) -> (u32, u32) {
    assert_eq!(&data[..2], &[0xFF, 0xD8], "a JPEG starts with SOI");
    assert_eq!(&data[data.len() - 2..], &[0xFF, 0xD9], "and ends with EOI");
    let mut i = 2;
    while i + 9 < data.len() {
        assert_eq!(data[i], 0xFF);
        let kind = data[i + 1];
        let len = usize::from(u16::from_be_bytes([data[i + 2], data[i + 3]]));
        if kind == 0xC0 || kind == 0xC2 {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]);
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]);
            return (u32::from(w), u32::from(h));
        }
        i += 2 + len;
    }
    panic!("no SOF segment");
}

#[test]
fn the_image_budget_lowers_the_quality_then_refuses() {
    let dir = TempDir::new("budget");
    let clip = grid(&dir.0, "grid", (1024, 512), 2, false);
    let args = json!({"file": clip, "views": [{"yaw": 0}, {"yaw": 90, "pitch": 30}]});
    let (r, images) = look(&dir.tools(), args.clone()).unwrap();
    assert_eq!(r["image"]["quality"], 3);
    let size = jpeg_size(&images[0].data);
    assert_eq!(
        json!([size.0, size.1]),
        json!([r["image"]["width"], r["image"]["height"]])
    );
    let b64 = images[0].data.len().div_ceil(3) * 4;

    let tight = dir.tools().image_budget(b64 - 1);
    let (r, smaller) = look(&tight, args.clone()).unwrap();
    assert!(r["image"]["quality"].as_u64().unwrap() > 3, "{r}");
    assert!(smaller[0].data.len() < images[0].data.len());

    let e = failed(&dir.tools().image_budget(500), args.clone());
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::IMAGE_TOO_LARGE, Some("size"))
    );
    assert!(e.next.unwrap_or_default().contains("--image-budget-kb"));

    let mut png = args;
    png["format"] = json!("png");
    let e = failed(&dir.tools().image_budget(b64), png);
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::IMAGE_TOO_LARGE, Some("format"))
    );
}

#[test]
fn output_writes_a_new_file_and_nothing_inline() {
    let dir = TempDir::new("output");
    let clip = grid(&dir.0, "grid", (1024, 512), 2, false);
    let tools = dir.writing();
    let args = json!({"file": clip, "output": "look.jpg", "size": 2000});
    let (r, images) = look(&tools, args.clone()).unwrap();
    assert!(images.is_empty());
    assert_eq!(r["image"]["path"], "look.jpg", "{r}");
    assert_eq!(r["image"]["width"], 2000);
    let data = std::fs::read(dir.0.join("look.jpg")).unwrap();
    assert_eq!(jpeg_size(&data).0, 2000);
    assert_eq!(r["image"]["bytes"], data.len());
    let e = failed(&tools, args);
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::OUTPUT_EXISTS, Some("output"))
    );
    assert_eq!(
        std::fs::read(dir.0.join("look.jpg")).unwrap(),
        data,
        "kept as it was"
    );

    // Without writing on, before any work.
    let e = failed(&dir.tools(), json!({"file": clip, "output": "other.jpg"}));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::WRITE_REFUSED, Some("output"))
    );
    assert!(!dir.0.join("other.jpg").exists());
}

#[test]
fn each_limit_names_its_argument() {
    let dir = TempDir::new("limits");
    let clip = grid(&dir.0, "grid", (256, 128), 30, false);
    let tools = dir.tools();
    let views = |n: usize| (0..n).map(|i| json!({"yaw": i * 10})).collect::<Vec<_>>();
    let rows: Vec<(Value, &str, &str)> = vec![
        (json!({"size": 2000}), codes::IMAGE_TOO_LARGE, "size"),
        (
            json!({"mode": "cube", "times": ["0", "1f", "2f", "3f", "4f"]}),
            codes::OUT_OF_RANGE,
            "times",
        ),
        (
            json!({"mode": "sheet", "times": ["0"]}),
            codes::BAD_ARGUMENT,
            "times",
        ),
        (
            json!({"views": views(2), "times": ["0", "1f"]}),
            codes::BAD_ARGUMENT,
            "views",
        ),
        (
            json!({"view": {}, "views": views(1)}),
            codes::BAD_ARGUMENT,
            "views",
        ),
        (
            json!({"mode": "sphere", "view": {"yaw": 3}}),
            codes::BAD_ARGUMENT,
            "view",
        ),
        (
            json!({"view": {"fov": 171}}),
            codes::OUT_OF_RANGE,
            "view.fov",
        ),
        (
            json!({"view": {"fov": 90, "vfov": 60}}),
            codes::BAD_ARGUMENT,
            "view.vfov",
        ),
        (
            json!({"views": [{"yaw": 1}, {"fov": 400}]}),
            codes::OUT_OF_RANGE,
            "views[1].fov",
        ),
        (
            json!({"views": [{"yaw": 1}, {"zoom": 2}]}),
            codes::BAD_ARGUMENT,
            "views[1]",
        ),
        (json!({"times": ["2"]}), codes::OUT_OF_RANGE, "times[0]"),
        (
            json!({"times": ["0", "40f"]}),
            codes::OUT_OF_RANGE,
            "times[1]",
        ),
        (json!({"times": [[1, 0]]}), codes::BAD_ARGUMENT, "times[0]"),
        (json!({"times": ["soon"]}), codes::BAD_ARGUMENT, "times[0]"),
        (json!({"times": [1.5]}), codes::BAD_ARGUMENT, "times[0]"),
        (
            json!({"times": ["0.5", "0.2"], "count": 3}),
            codes::BAD_ARGUMENT,
            "times[1]",
        ),
        (
            json!({"times": ["0"], "count": 3}),
            codes::BAD_ARGUMENT,
            "times",
        ),
        (json!({"output": "a.png"}), codes::BAD_ARGUMENT, "output"),
        // 24 tiles of 112 pixels don't fit a 256-pixel image.
        (
            json!({"mode": "sheet", "count": 24, "size": 256}),
            codes::OUT_OF_RANGE,
            "size",
        ),
    ];
    for (extra, code, arg) in rows {
        let mut args = json!({"file": clip});
        for (k, v) in extra.as_object().unwrap() {
            args[k] = v.clone();
        }
        let e = failed(&tools, args.clone());
        assert_eq!(e.code, code, "{args}: {e:?}");
        let at = e.argument.clone().unwrap_or_default();
        assert!(at.starts_with(arg), "{args}: {e:?}");
        let json = e.to_json().to_string();
        assert!(
            !json.contains("grid.mp4"),
            "no file name in an error: {json}"
        );
    }
    // Without writing, the way to a larger image is the user's flag.
    let e = failed(&tools, json!({"file": clip, "size": 2000}));
    let next = e.next.clone().unwrap_or_default();
    assert!(next.contains("--allow-write"), "{e:?}");
    let e = failed(&tools, json!({"file": "missing.mp4"}));
    assert_eq!(
        (e.code, e.argument.as_deref()),
        (codes::NOT_FOUND, Some("file"))
    );
}

#[test]
fn a_call_out_of_time_stops() {
    let dir = TempDir::new("time");
    let clip = grid(&dir.0, "grid", (256, 128), 2, false);
    let cx = CallCx::standalone(Instant::now());
    let e = dir
        .tools()
        .call("look", json!({"file": clip}), &cx)
        .err()
        .unwrap();
    assert_eq!(e.code, codes::TIMED_OUT, "{e:?}");
}
