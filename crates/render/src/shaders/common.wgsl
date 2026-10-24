// Shared by every pass: one triangle that covers the target, so each
// fragment is one output texel and @builtin(position).xy is its centre.

const PI: f32 = 3.14159265358979;
const TAU: f32 = 6.28318530717959;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// `v` rounded to the nearest IEEE half, ties to even, as an f32. Every
// pass that writes an f16 target returns this, so that the store is exact
// whatever the driver's own conversion does: lavapipe's truncates
// (DECISIONS D31), and Vulkan lets a driver round either way.
fn round_f16(v: vec4<f32>) -> vec4<f32> {
    // A normal half keeps 10 of the f32's 23 mantissa bits: add just under
    // half of the 13 dropped ones, plus the lowest kept bit, so that a tie
    // carries only into an odd one, then clear them.
    let b = bitcast<vec4<u32>>(v);
    let lsb = (b >> vec4<u32>(13u)) & vec4<u32>(1u);
    let normal = bitcast<vec4<f32>>((b + vec4<u32>(0xfffu) + lsb) & vec4<u32>(0xffffe000u));
    // Below 2^-14 the halves are the multiples of 2^-24; scaling by a
    // power of two is exact, and `round` ties to even.
    let subnormal = round(v * 16777216.0) / 16777216.0;
    // From 65520 up the nearest half is infinite. Rounded as a normal it
    // would be 65536, which isn't a half, and lavapipe stores the largest
    // half for it; an infinity, made from bits, is stored as it is.
    let inf = bitcast<vec4<f32>>((b & vec4<u32>(0x80000000u)) | vec4<u32>(0x7f800000u));
    let r = select(normal, subnormal, abs(v) < vec4<f32>(6.103515625e-5));
    return select(r, inf, abs(normal) >= vec4<f32>(65536.0));
}
