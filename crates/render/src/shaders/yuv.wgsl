// The YUV pass (M2 plan section 7.3): the linear target into the
// encoder's Y'CbCr, in a storage buffer laid out as the encoder takes it.
// Per pixel the sRGB OETF (encode.wgsl's, so an export matches the
// viewer), then BT.709's matrix in limited range; chroma 4:2:0 with left
// siting, the taps 1/4, 1/2, 1/4 about the even column and the two rows
// averaged; every sample rounded to the nearest code, ties to even.
// spherecut_render::yuv::reference does the same in f64.

struct Yuv {
    // Each plane's first word in `out`, and its row's length in words.
    offsets: vec4<u32>,
    strides: vec4<u32>,
    // The picture's width and height in pixels.
    size: vec2<u32>,
    // 8 or 10.
    bits: u32,
    // 0 Y then interleaved CbCr (NV12, P010); 1 Y, Cb, Cr (I420, I010).
    planar: u32,
    // Where a 10-bit code sits in its u16: 6 (P010, the top bits) or 0.
    shift: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var linear: texture_2d<f32>;
@group(0) @binding(1) var<uniform> yuv: Yuv;
@group(0) @binding(2) var<storage, read_write> out: array<u32>;

fn srgb_oetf(l: vec3<f32>) -> vec3<f32> {
    let lo = l * 12.92;
    let hi = 1.055 * pow(l, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, l <= vec3<f32>(0.0031308));
}

// R'G'B' at (x, y), the edges repeated.
fn encoded(x: i32, y: i32) -> vec3<f32> {
    let p = clamp(vec2<i32>(x, y), vec2<i32>(0), vec2<i32>(yuv.size) - vec2<i32>(1));
    let c = textureLoad(linear, p, 0).rgb;
    return srgb_oetf(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)));
}

fn luma_of(e: vec3<f32>) -> f32 {
    return dot(e, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Cb' and Cr', each in [−½, ½].
fn chroma_of(e: vec3<f32>) -> vec2<f32> {
    let y = luma_of(e);
    return vec2<f32>((e.b - y) / 1.8556, (e.r - y) / 1.5748);
}

// 1 at 8 bits, 4 at 10.
fn scale() -> f32 {
    return select(1.0, 4.0, yuv.bits == 10u);
}

fn code(v: f32) -> u32 {
    return u32(clamp(round(v), 0.0, select(255.0, 1023.0, yuv.bits == 10u)));
}

fn luma_code(x: i32, y: i32) -> u32 {
    return code((16.0 + 219.0 * luma_of(encoded(x, y))) * scale());
}

// Cb and Cr of chroma sample (cx, cy): luma column 2cx and its two
// neighbours, rows 2cy and 2cy + 1.
fn chroma_codes(cx: i32, cy: i32) -> vec2<u32> {
    var s = vec2<f32>(0.0);
    for (var r = 0; r < 2; r++) {
        let y = 2 * cy + r;
        let x = 2 * cx;
        s += 0.25 * chroma_of(encoded(x - 1, y)) + 0.5 * chroma_of(encoded(x, y))
            + 0.25 * chroma_of(encoded(x + 1, y));
    }
    let c = (128.0 + 224.0 * s * 0.5) * scale();
    return vec2<u32>(code(c.x), code(c.y));
}

// Sample `k` of a word holding 4 (8-bit) or 2 (10-bit) of them.
fn place(v: u32, k: u32) -> u32 {
    if yuv.bits == 8u {
        return v << (8u * k);
    }
    return (v << yuv.shift) << (16u * k);
}

fn per_word() -> u32 {
    return select(2u, 4u, yuv.bits == 8u);
}

// One word of a luma row; past the picture's width, the row's padding,
// zero.
@compute @workgroup_size(64, 1, 1)
fn cs_luma(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= yuv.strides.x || id.y >= yuv.size.y {
        return;
    }
    var word = 0u;
    for (var k = 0u; k < per_word(); k++) {
        let x = id.x * per_word() + k;
        if x < yuv.size.x {
            word |= place(luma_code(i32(x), i32(id.y)), k);
        }
    }
    out[yuv.offsets.x + id.y * yuv.strides.x + id.x] = word;
}

// One word of a chroma row: interleaved Cb, Cr pairs, or a word of each
// of the Cb and Cr planes.
@compute @workgroup_size(64, 1, 1)
fn cs_chroma(@builtin(global_invocation_id) id: vec3<u32>) {
    let cw = yuv.size.x / 2u;
    if id.x >= yuv.strides.y || id.y >= yuv.size.y / 2u {
        return;
    }
    let row = id.y * yuv.strides.y + id.x;
    if yuv.planar == 0u {
        // A pair is two samples: 2 pairs a word at 8 bits, 1 at 10.
        let pairs = per_word() / 2u;
        var word = 0u;
        for (var k = 0u; k < pairs; k++) {
            let cx = id.x * pairs + k;
            if cx < cw {
                let c = chroma_codes(i32(cx), i32(id.y));
                word |= place(c.x, 2u * k) | place(c.y, 2u * k + 1u);
            }
        }
        out[yuv.offsets.y + row] = word;
        return;
    }
    var cb = 0u;
    var cr = 0u;
    for (var k = 0u; k < per_word(); k++) {
        let cx = id.x * per_word() + k;
        if cx < cw {
            let c = chroma_codes(i32(cx), i32(id.y));
            cb |= place(c.x, k);
            cr |= place(c.y, k);
        }
    }
    out[yuv.offsets.y + row] = cb;
    out[yuv.offsets.z + row] = cr;
}
