// The linear target to display values: the sRGB OETF, one texel per
// output pixel, with no filtering (DECISIONS D31). `origin` is where the
// target's texel (0, 0) sits in the output, so the pass can draw into a
// rectangle of a larger frame (egui's, from slice 7); `extent` is the
// part of the target the projection drew, and outside it is black.

// The output's largest code, 2^bits − 1 (`encode::code_max`): 255 for an
// 8-bit unorm target, 1023 for a 10-bit one, or 0 for a float target,
// which takes the OETF as it is. The pass rounds to the codes itself and
// returns k / CODE_MAX, which leaves the driver's conversion nothing to
// round. Left to the driver, the OETF's own value came out a code low on
// RADV wherever it lay less than 0.1245 codes above a tie, since RADV
// packs a fragment's output to f16 toward zero before its unorm
// conversion; k / 255 packed so lies less than 0.1245 codes below k, at
// least 0.375 codes from either tie.
override CODE_MAX: f32;

// Whether the pass returns the nearest half of that
// (`encode::returns_half`). At 10 bits a half's step is half a code, and
// k / 1023 packed toward zero can land 0.0005 codes above the tie below
// k, so the pass returns its nearest half, within 0.25 codes of k, which
// every export keeps as it is. Into an f16 target the half is what the
// store must hold on any driver, as for every pass that writes f16
// (DECISIONS D31): lavapipe's own conversion truncates.
override HALF: bool;

struct Place {
    origin: vec2<i32>,
    extent: vec2<i32>,
};

@group(0) @binding(0) var linear: texture_2d<f32>;
@group(0) @binding(1) var<uniform> place: Place;

fn srgb_oetf(l: vec3<f32>) -> vec3<f32> {
    let lo = l * 12.92;
    let hi = 1.055 * pow(l, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, l <= vec3<f32>(0.0031308));
}

// `v` as the nearest code over CODE_MAX, ties to even (`round`), or as it
// is for a float target; then its nearest half, if HALF.
fn to_output(v: vec3<f32>) -> vec3<f32> {
    var out = v;
    if CODE_MAX != 0.0 {
        out = round(v * CODE_MAX) / CODE_MAX;
    }
    if HALF {
        out = round_f16(vec4<f32>(out, 1.0)).xyz;
    }
    return out;
}

@fragment
fn fs_encode(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(frag.xy)) - place.origin;
    if any(p < vec2<i32>(0)) || any(p >= place.extent) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let c = textureLoad(linear, p, 0);
    let v = srgb_oetf(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    return vec4<f32>(to_output(v), 1.0);
}
