// The linear target to display values: the sRGB OETF, one texel per
// output pixel, with no filtering (DECISIONS D31). `origin` is where the
// target's texel (0, 0) sits in the output, so the pass can draw into a
// rectangle of a larger frame (egui's, from slice 7); `extent` is the
// part of the target the projection drew, and outside it is black.

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

@fragment
fn fs_encode(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(frag.xy)) - place.origin;
    if any(p < vec2<i32>(0)) || any(p >= place.extent) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let c = textureLoad(linear, p, 0);
    return vec4<f32>(srgb_oetf(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0))), 1.0);
}
