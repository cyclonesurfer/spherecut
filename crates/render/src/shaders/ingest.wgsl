// A 9- to 16-bit plane, uploaded as integers, into its canonical f16
// texture as code / (2^b − 1) rounded to the nearest half, ties to even
// (DECISIONS D31). P010's bits are the high ones, so its codes shift down
// by 6; LSB-aligned formats shift by 0. A code past 2^b − 1 (stray high
// bits) is clamped.

struct Ingest {
    shift: u32,
    max_code: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var scratch: texture_2d<u32>;
@group(0) @binding(1) var<uniform> ing: Ingest;

// code / max as the nearest half, ties to even, worked out in integers,
// so that neither the GPU's division nor its log2 can move a code that
// lies near a tie (12-bit codes come within an f32 ulp of one). The
// result is exactly a half, which the f16 target stores as it is.
fn code_to_half(code: u32, max: u32) -> f32 {
    if code == 0u {
        return 0.0;
    }
    // 2^e ≤ code / max < 2^(e + 1), with e ≤ 0 since code ≤ max.
    var e = i32(firstLeadingBit(code)) - i32(firstLeadingBit(max));
    if (code << u32(-e)) < max {
        e -= 1;
    }
    // The half is q · 2^-s, with q = code · 2^s / max rounded: s = 10 − e
    // for a normal half, 24 for a subnormal one (below 2^-14). code · 2^s
    // stays below max · 2^11, so it fits in a u32.
    let s = u32(select(10 - e, 24, e < -14));
    let n = code << s;
    var q = n / max;
    let r2 = 2u * (n - q * max);
    if r2 > max || (r2 == max && (q & 1u) == 1u) {
        q += 1u;
    }
    return ldexp(f32(q), -i32(s));
}

@fragment
fn fs_ingest(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(scratch, vec2<u32>(frag.xy), 0);
    let v = min(c.xy >> vec2<u32>(ing.shift), vec2<u32>(ing.max_code));
    // R16Uint and Rg16Uint: one or two components.
    return vec4<f32>(code_to_half(v.x, ing.max_code), code_to_half(v.y, ing.max_code), 0.0, 1.0);
}
