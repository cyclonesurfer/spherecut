// The linear target to display values: the sRGB OETF, one texel per
// output pixel, with no filtering (DECISIONS D31). `origin` is where the
// target's texel (0, 0) sits in the output, so the pass can draw into a
// rectangle of a larger frame (egui's, from slice 7); `extent` is the
// part of the target the projection drew, and outside it is black.

// The output's largest code, 2^bits − 1 (`encode::code_max`): 255 for an
// 8-bit unorm target, 1023 for a 10-bit one, or 0 for a float target,
// which takes the OETF as it is. The pass rounds to the codes itself and
// returns k / CODE_MAX, so that the store gives k on any driver. Left to
// the driver, the OETF's own value came out a code low on RADV wherever
// it lay less than 0.1245 codes above a tie, since RADV packs a
// fragment's output to f16 toward zero before its unorm conversion;
// k / 255 packed so is less than 0.1245 codes below k, and k / 1023 less
// than 0.4995, so rounding to nearest still gives k.
override CODE_MAX: f32;

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

// `v` as the nearest code over CODE_MAX, ties to even (`round`); as it
// is for a float target.
fn to_codes(v: vec3<f32>) -> vec3<f32> {
    if CODE_MAX == 0.0 {
        return v;
    }
    return round(v * CODE_MAX) / CODE_MAX;
}

@fragment
fn fs_encode(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(frag.xy)) - place.origin;
    if any(p < vec2<i32>(0)) || any(p >= place.extent) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let c = textureLoad(linear, p, 0);
    let v = srgb_oetf(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    return vec4<f32>(to_codes(v), 1.0);
}
