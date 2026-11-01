//! `synth::code`: the frame codes in the corners of a lens image (M3 plan
//! 7.1), written into pictures and read back from their planes, decoded
//! through lossless and lossy x265.
//!
//! Every code word of both lenses is checked, so is the code's distance,
//! which makes any one, two or three cells read wrong fail the checksum.
//! The pictures are read from synth's own planes at 8 and 10 bits, from
//! little-endian rows with padding in both alignments, and decoded from
//! two-track files coded as the perf clips are. The bound that keeps a
//! block off a rendered lens's image circle is checked against the
//! blocks' pixels.
//!
//! Budget (M3 plan 4.1 rule 5): 5 s for the suite, which each test
//! prints its time against.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use spherecut_core::StopToken;
use spherecut_media::backend::{FfmpegBackend, MediaBackend, NextFrame, VideoRequest};
use spherecut_media::encode::ColorTags;
use spherecut_synth::code::{
    self, CHECK_XOR, CodeError, Corner, CornerCode, Luma, Samples, block_bound, block_side,
    check_clear, clearance, code_word, corner_code, read_corner_code, read_corners, read_word,
};
use spherecut_synth::media::{self, CLIP_COLOR, Codec, Gop, LENS_COLOR, PairSpec, Spec};
use spherecut_synth::yuv::{self, Chroma, YuvFormat, YuvPlanes};
use spherecut_synth::{Pattern, RgbImage, render_equirect};
use spherecut_testkit::control::must_fail;

/// The suite's budget in seconds.
const BUDGET_S: f64 = 5.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the suite's budget. The tests run
/// side by side, so none of them can time the rest: the last line
/// printed is the suite's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the suite {suite:.3} s so far, of its {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// The test's own directory, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn temp_dir(name: &str) -> TempDir {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// Every 25-bit word with one, two or three bits set.
fn light_words() -> Vec<u32> {
    let mut words = Vec::new();
    for i in 0..25 {
        words.push(1 << i);
        for j in i + 1..25 {
            words.push(1 << i | 1 << j);
            for k in j + 1..25 {
                words.push(1 << i | 1 << j | 1 << k);
            }
        }
    }
    words
}

/// The moving grid at `side`², frame `frame`, with the corner codes of
/// that frame of lens `lens`.
fn coded(side: u32, lens: usize, frame: u32) -> RgbImage {
    let mut img = render_equirect(Pattern::Grid, side, side, frame, 1);
    corner_code(&mut img, lens, frame).unwrap();
    img
}

/// `img` in 4:2:0 at `bits`, in the range and matrix `color` tags.
fn planes(img: &RgbImage, bits: u32, color: &ColorTags) -> YuvPlanes {
    yuv::to_yuv(img, bits, &YuvFormat::tagged(color, Chroma::Yuv420))
}

/// The luma of `planes` as little-endian rows of `stride` samples, the
/// padding filled with a pattern, and each value shifted up into the
/// high bits when `msb_aligned` (P010).
fn le_rows(planes: &YuvPlanes, side: u32, stride: usize, bits: u32, msb_aligned: bool) -> Vec<u8> {
    let YuvPlanes::U16(p) = planes else {
        panic!("not 16-bit planes")
    };
    let side = side as usize;
    let mut out = vec![0xab; stride * side * 2];
    for (y, row) in p[0].chunks(side).enumerate() {
        for (x, &v) in row.iter().enumerate() {
            let v = if msb_aligned { v << (16 - bits) } else { v };
            out[2 * (y * stride + x)..][..2].copy_from_slice(&v.to_le_bytes());
        }
    }
    out
}

#[test]
fn every_code_word_reads_back_and_a_flipped_bit_fails() {
    let _time = timed("every_code_word_reads_back_and_a_flipped_bit_fails");
    let zero = code_word(CornerCode::of(0, 0)).unwrap();
    let mut nearest = u32::MAX;
    for lens in 0..code::LENSES {
        for frame in 0..1u32 << code::FRAME_BITS {
            let code = CornerCode::of(lens, frame);
            let word = code_word(code).unwrap();
            assert_eq!(word >> 25, 0, "{code:?}");
            assert_eq!(read_word(word), Some(code));
            for k in 0..25 {
                assert_eq!(read_word(word ^ 1 << k), None, "{code:?}, bit {k}");
            }
            // The checksum is linear in the bits it covers, and the XOR
            // is the same in every word, so the words' least distance
            // apart is the least weight of a difference from frame 0 of
            // lens 0.
            if (lens, frame) != (0, 0) {
                nearest = nearest.min((word ^ zero).count_ones());
            }
        }
    }
    assert_eq!(nearest, 4, "CRC-8's distance over 17 bits");
    // So any one, two or three cells read wrong fail the checksum; on a
    // few words, each such pattern.
    let patterns = light_words();
    assert_eq!(patterns.len(), 25 + 300 + 2300);
    for code in [(0, 0), (1, 0xffff), (0, 599), (1, 1234)].map(|(l, f)| CornerCode::of(l, f)) {
        let word = code_word(code).unwrap();
        assert!(
            patterns.iter().all(|e| read_word(word ^ e).is_none()),
            "{code:?}"
        );
    }
    // A block all black or all white holds no code, nor do bits past 25.
    assert_eq!(zero, u32::from(CHECK_XOR) << 17);
    assert_eq!(read_word(0), None);
    assert_eq!(read_word((1 << 25) - 1), None);
    assert_eq!(read_word(zero | 1 << 25), None);
    assert_eq!(code_word(CornerCode::of(2, 0)), Err(CodeError::Lens(2)));
    // Frames count modulo 2¹⁶.
    assert_eq!(
        CornerCode::of(1, 65_536 + 7),
        CornerCode { lens: 1, frame: 7 }
    );

    // Its control: the word of frame 1234 of lens 1 with cell 7 flipped.
    let word = code_word(CornerCode::of(1, 1234)).unwrap();
    must_fail(
        "cell 7 flipped read as a code",
        read_word(word ^ 1 << 7).ok_or("the checksum fails"),
    );
}

#[test]
fn codes_read_back_from_8_and_10_bit_planes() {
    let _time = timed("codes_read_back_from_8_and_10_bit_planes");
    for side in [60, 64, 384, 768] {
        for (lens, frame) in [(0, 0), (1, 1), (0, 599), (1, 65_535), (0, 65_536 + 12)] {
            let want = CornerCode::of(lens, frame);
            let img = coded(side, lens, frame);
            for (color, range) in [(&LENS_COLOR, "full"), (&CLIP_COLOR, "limited")] {
                for bits in [8, 10] {
                    let p = planes(&img, bits, color);
                    let luma = Luma::of_yuv(&p, side, side, bits);
                    let at = format!("{side}², {bits} bits, {range} range, {want:?}");
                    assert_eq!(read_corner_code(&luma), Ok(want), "{at}");
                    assert_eq!(read_corners(&luma).unwrap(), [Corner::Code(want); 4]);
                    // Black and white each lie at least 0.29 of the range
                    // outside the band that reads as neither: 3/8 in full
                    // range, less what limited range leaves out.
                    let clear = clearance(&luma).unwrap();
                    assert!(clear > 0.29, "{at}: {clear}");
                    if bits == 10 {
                        for msb_aligned in [false, true] {
                            let stride = side as usize + 24;
                            let bytes = le_rows(&p, side, stride, bits, msb_aligned);
                            let luma = Luma {
                                samples: Samples::Le16 {
                                    bytes: &bytes,
                                    msb_aligned,
                                },
                                stride,
                                ..luma
                            };
                            assert_eq!(read_corner_code(&luma), Ok(want), "{at}, {msb_aligned}");
                        }
                    }
                }
            }
        }
    }

    // What isn't a plane to read is refused.
    let p = planes(&coded(64, 0, 3), 8, &LENS_COLOR);
    let luma = Luma::of_yuv(&p, 64, 64, 8);
    let refused = |l: Luma<'_>| read_corner_code(&l).unwrap_err();
    assert!(matches!(
        refused(Luma { bits: 7, ..luma }),
        CodeError::Plane(_)
    ));
    assert!(matches!(
        refused(Luma { stride: 63, ..luma }),
        CodeError::Plane(_)
    ));
    assert!(matches!(
        refused(Luma { height: 65, ..luma }),
        CodeError::Plane(_)
    ));
    assert_eq!(
        refused(Luma { height: 32, ..luma }),
        CodeError::NotSquare {
            width: 64,
            height: 32
        }
    );
    assert_eq!(
        refused(Luma {
            width: 48,
            height: 48,
            ..luma
        }),
        CodeError::TooSmall { side: 48, block: 4 }
    );
    let mut small = render_equirect(Pattern::Grid, 48, 48, 0, 1);
    assert_eq!(
        corner_code(&mut small, 0, 0),
        Err(CodeError::TooSmall { side: 48, block: 4 })
    );

    // Its control: P010's samples read as if their value were in the low
    // bits saturate, every cell reads white, and no corner holds a code.
    let img = coded(384, 1, 77);
    let p = planes(&img, 10, &CLIP_COLOR);
    let bytes = le_rows(&p, 384, 400, 10, true);
    let wrong = Luma {
        samples: Samples::Le16 {
            bytes: &bytes,
            msb_aligned: false,
        },
        width: 384,
        height: 384,
        stride: 400,
        bits: 10,
    };
    let e = must_fail("P010 read as low-aligned", read_corner_code(&wrong));
    assert_eq!(
        e,
        CodeError::NoCode(
            [Corner::Checksum {
                word: (1 << 25) - 1
            }; 4]
        )
    );
}

/// Paint the block of corner `corner` of `img` from `from`'s.
fn copy_block(img: &mut RgbImage, from: &RgbImage, corner: usize) {
    let (side, b) = (img.width, block_side(img.width));
    let far = side - b;
    let (x0, y0) = [(0, 0), (far, 0), (0, far), (far, far)][corner];
    for y in y0..y0 + b {
        for x in x0..x0 + b {
            let i = (y * side + x) as usize;
            img.pixels[i] = from.pixels[i];
        }
    }
}

/// `img` with corner `corner`'s block painted `rgb`.
fn fill_block(img: &mut RgbImage, corner: usize, rgb: [f64; 3]) {
    let side = img.width;
    let grey = RgbImage {
        width: side,
        height: side,
        pixels: vec![rgb; (side * side) as usize],
    };
    copy_block(img, &grey, corner);
}

/// What `img`'s corners read as, in 8-bit full range.
fn read_img(img: &RgbImage) -> (Result<CornerCode, CodeError>, [Corner; 4]) {
    let p = planes(img, 8, &LENS_COLOR);
    let luma = Luma::of_yuv(&p, img.width, img.height, 8);
    (read_corner_code(&luma), read_corners(&luma).unwrap())
}

#[test]
fn a_lost_corner_leaves_the_others_and_corners_that_disagree_are_refused() {
    let _time = timed("a_lost_corner_leaves_the_others_and_corners_that_disagree_are_refused");
    let want = CornerCode::of(1, 1234);
    let word = code_word(want).unwrap();
    let good = coded(384, 1, 1234);

    // Grey over the top left corner: its first cell reads as neither.
    let mut img = good.clone();
    fill_block(&mut img, 0, [0.5; 3]);
    let (read, corners) = read_img(&img);
    assert_eq!(read, Ok(want));
    assert_eq!(corners[0], Corner::Unclear { cell: 0 });
    // The next frame's block over the top right, with one cell flipped:
    // its checksum fails.
    let mut flipped = good.clone();
    corner_code(&mut flipped, 1, 1235).unwrap();
    let cell = cell_rect(384, 1, 3);
    flip_cell(&mut flipped, cell);
    copy_block(&mut img, &flipped, 1);
    // White over the bottom left.
    fill_block(&mut img, 2, [1.0; 3]);
    let (read, corners) = read_img(&img);
    assert_eq!(read, Ok(want), "{corners:?}");
    let next = code_word(CornerCode::of(1, 1235)).unwrap();
    assert_eq!(
        corners,
        [
            Corner::Unclear { cell: 0 },
            Corner::Checksum {
                word: next ^ 1 << 3
            },
            Corner::Checksum {
                word: (1 << 25) - 1
            },
            Corner::Code(want),
        ]
    );

    // Corners that hold different codes are refused, whichever is right.
    let mut other = good.clone();
    corner_code(&mut other, 0, 1234).unwrap();
    let mut mixed = good.clone();
    copy_block(&mut mixed, &other, 3);
    let e = must_fail("corners holding two codes", read_img(&mixed).0);
    assert_eq!(
        e,
        CodeError::Disagree([
            Corner::Code(want),
            Corner::Code(want),
            Corner::Code(want),
            Corner::Code(CornerCode::of(0, 1234)),
        ])
    );

    // Its control: with the last corner lost too, there is no code; and
    // one cell flipped in every corner fails every checksum.
    fill_block(&mut img, 3, [0.0; 3]);
    let e = must_fail("all four corners lost", read_img(&img).0);
    assert!(matches!(e, CodeError::NoCode(_)), "{e:?}");
    let mut each = good.clone();
    for corner in 0..4 {
        flip_cell(&mut each, cell_rect(384, corner, 17));
    }
    let e = must_fail("cell 17 flipped in every corner", read_img(&each).0);
    assert_eq!(
        e,
        CodeError::NoCode(
            [Corner::Checksum {
                word: word ^ 1 << 17
            }; 4]
        )
    );
}

/// The pixels of cell `k` of corner `corner`'s block in a picture `side`
/// px square: columns and rows from and to.
fn cell_rect(side: u32, corner: usize, k: u32) -> (u32, u32, u32, u32) {
    let b = block_side(side);
    let far = side - b;
    let (x0, y0) = [(0, 0), (far, 0), (0, far), (far, far)][corner];
    let span = |o: u32, i: u32| (o + i * b / code::GRID, o + (i + 1) * b / code::GRID);
    let (xa, xb) = span(x0, k % code::GRID);
    let (ya, yb) = span(y0, k / code::GRID);
    (xa, xb, ya, yb)
}

/// Turn a cell's black white, or its white black.
fn flip_cell(img: &mut RgbImage, (xa, xb, ya, yb): (u32, u32, u32, u32)) {
    for y in ya..yb {
        for x in xa..xb {
            let px = &mut img.pixels[(y * img.width + x) as usize];
            *px = px.map(|c| 1.0 - c);
        }
    }
}

/// Every frame of each track of `path`, decoded in software, read for
/// its corner code: frame i of track k must hold lens k's frame i. The
/// least clearance over the frames.
fn read_back(path: &Path, frames: [u32; 2], lens_of_track: [usize; 2]) -> Result<f64, String> {
    let never = StopToken::never();
    let mut least = f64::INFINITY;
    for k in 0..2 {
        let req = VideoRequest {
            stream: Some(k),
            ..VideoRequest::default()
        };
        let mut dec = FfmpegBackend::default().open_video(path, &req).unwrap();
        let mut i = 0;
        loop {
            match dec.next_frame(&never).unwrap() {
                NextFrame::Frame(f) => {
                    let luma = Luma::of_picture(&*f).unwrap();
                    let read = read_corner_code(&luma);
                    let want = CornerCode::of(lens_of_track[k], i);
                    if read != Ok(want) {
                        return Err(format!("track {k}, frame {i}: {read:?}, not {want:?}"));
                    }
                    least = least.min(clearance(&luma).unwrap());
                    i += 1;
                }
                NextFrame::End => break,
                NextFrame::Stopped => panic!("stopped"),
            }
        }
        assert_eq!(i, frames[k], "track {k}");
    }
    Ok(least)
}

/// A pair of `side`² lenses of `frames` frames each, `codec` at `bits`,
/// with an IDR every 6 frames, noise, and corner codes.
fn pair(
    name: &str,
    side: u32,
    frames: u32,
    codec: Codec,
    bits: u32,
    kbps: Option<u32>,
) -> PairSpec {
    let lens = |k: u64| Spec {
        name: format!("{name}-lens{k}"),
        width: side,
        height: side,
        frames,
        codec,
        bits,
        color: LENS_COLOR,
        gop: Some(Gop {
            keyint: 6,
            bframes: 0,
            open: false,
            radl: 0,
        }),
        noise: Some(0x1e45 + k),
        bitrate_kbps: kbps,
        ..Spec::default()
    };
    PairSpec {
        name: name.into(),
        lenses: [lens(0), lens(1)],
        corner_codes: true,
        threaded: false,
    }
}

#[test]
fn codes_round_trip_through_lossless_and_lossy_x265() {
    let _time = timed("codes_round_trip_through_lossless_and_lossy_x265");
    let dir = temp_dir("code-x265");
    // The perf clips' coding at 384², 2 × 24 frames: 2×3840² at 65 Mbps
    // a lens is 650 kbps here, the fewest bits a pixel of the three, with
    // their noise, GOP and threads.
    let perf = media::perf_pairs()[0].scaled(384, 24);
    assert_eq!(perf.lenses[0].bitrate_kbps, Some(650));
    // At 64² a cell is a pixel, which the perf clips' bits a pixel (18
    // kbps here) blur away: 100 kbps keeps it.
    let cases = [
        (
            pair("lossless8", 64, 12, Codec::HevcLossless, 8, None),
            0.35,
        ),
        (
            pair("lossless10", 64, 12, Codec::HevcLossless, 10, None),
            0.35,
        ),
        (pair("lossy8", 64, 12, Codec::Hevc, 8, Some(100)), 0.12),
        (pair("lossy10", 64, 12, Codec::Hevc, 10, Some(100)), 0.12),
        (perf, 0.25),
    ];
    for (spec, floor) in &cases {
        let path = media::write_pair(spec, &dir.0).unwrap();
        let frames = spec.lenses.clone().map(|l| l.frames);
        let clear = read_back(&path, frames, [0, 1]).unwrap();
        eprintln!(
            "{}: every frame of both lenses reads, clear by {clear:.3} of the range ({:.0} \
             levels at 8 bits)",
            spec.name,
            clear * 255.0
        );
        assert!(clear > *floor, "{}: {clear}", spec.name);
    }

    // Its control: each track read as the other lens fails at its first
    // frame, so the codes tell the lenses apart wherever they decode.
    let path = dir.0.join(cases[4].0.file_name());
    let why = must_fail("the lenses read swapped", read_back(&path, [24; 2], [1, 0]));
    assert!(why.starts_with("track 0, frame 0:"), "{why}");
}

#[test]
fn the_bound_keeps_blocks_off_the_image_circle() {
    let _time = timed("the_bound_keeps_blocks_off_the_image_circle");
    // The nearest point of each block's pixels to a circle's centre at
    // (cx, cy), and whether every block lies at least 2 px outside the
    // circle of radius r.
    let outside = |side: u32, (cx, cy): (f64, f64), r: f64| -> Result<(), String> {
        let b = f64::from(block_side(side));
        let far = f64::from(side) - b;
        for (x0, y0) in [(0.0, 0.0), (far, 0.0), (0.0, far), (far, far)] {
            let (nx, ny) = (cx.clamp(x0, x0 + b), cy.clamp(y0, y0 + b));
            let d = (nx - cx).hypot(ny - cy);
            if d < r + 2.0 - 1e-9 {
                return Err(format!(
                    "the block at ({x0}, {y0}) is {d:.3} px from the centre"
                ));
            }
        }
        Ok(())
    };
    let mut checked = 0;
    for side in [64, 384, 768, 1920, 2880, 3840] {
        let b = f64::from(block_side(side));
        for offset in [0.0, 1.5, 7.0, 40.0] {
            // The largest radius the bound allows with this offset, less
            // a little for rounding.
            let r = (f64::from(side) / 2.0 - offset - b) * std::f64::consts::SQRT_2 - 2.0 - 1e-6;
            assert!((block_bound(side, offset, r) - b).abs() < 1e-6);
            assert_eq!(check_clear(side, offset, r), Ok(()));
            assert_eq!(check_clear(side, -offset, r), Ok(()));
            for step in 0..16 {
                let a = f64::from(step) * std::f64::consts::TAU / 16.0;
                let c = (
                    f64::from(side) / 2.0 + offset * a.cos(),
                    f64::from(side) / 2.0 + offset * a.sin(),
                );
                outside(side, c, r).unwrap();
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 6 * 4 * 16);
    // At 384² with the circle centred, a radius of 224 px leaves room for
    // the 32 px blocks and 225 doesn't.
    assert_eq!(check_clear(384, 0.0, 224.0), Ok(()));
    let e = check_clear(384, 0.0, 225.0).unwrap_err();
    assert!(
        matches!(e, CodeError::ReachesCircle { block: 32, bound, .. } if bound < 32.0),
        "{e:?}"
    );
    assert!(check_clear(384, f64::NAN, 100.0).is_err());

    // Its control: centred, the bound is tight along the diagonals, so a
    // radius a pixel past it brings the circle within 2 px of a block.
    let r = (192.0 - 32.0) * std::f64::consts::SQRT_2 - 2.0 + 1.0;
    assert!(check_clear(384, 0.0, r).is_err());
    must_fail(
        "a radius a pixel past the bound",
        outside(384, (192.0, 192.0), r),
    );
}

/// The bytes a code paints over a mid-grey square of each side in
/// `sides`, a byte a pixel (0 black, 1 grey, 2 white), then the side of
/// the blocks at each of `blocks`.
fn painted(lens: usize, frame: u32, sides: &[u32], blocks: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for &side in sides {
        let mut img = RgbImage {
            width: side,
            height: side,
            pixels: vec![[0.5; 3]; (side * side) as usize],
        };
        corner_code(&mut img, lens, frame).unwrap();
        bytes.extend(img.pixels.iter().map(|p| (p[0] * 2.0).round() as u8));
    }
    for &side in blocks {
        bytes.extend(block_side(side).to_le_bytes());
    }
    bytes
}

/// The layout `code::FORMAT` names, pinned: the pixels lens 1's code of
/// frame 0xa5c3 paints at 64² and 384², and the blocks' side at the perf
/// pairs' 2880² and 3840², hashed. A change to the layout fails this
/// until `FORMAT` is raised and the pin moved with it, so that hw-test
/// makes the perf pairs again rather than taking those of the old layout
/// as made.
#[test]
fn the_layout_code_format_names_is_pinned() {
    let _time = timed("the_layout_code_format_names_is_pinned");
    let fingerprint =
        |lens, frame| spherecut_testkit::fnv64(&painted(lens, frame, &[64, 384], &[2880, 3840]));
    let pinned = fingerprint(1, 0xa5c3);
    eprintln!("corner codes: format {}, {pinned:#018x}", code::FORMAT);
    assert_eq!((code::FORMAT, pinned), (1, 0xd4f0_449e_dd74_0f0e));
    // Its control: a code one frame on paints other pixels, so the pin
    // sees the cells.
    must_fail(
        "the next frame's code taken for the pinned one",
        fingerprint(1, 0xa5c4) == pinned,
    );
}
