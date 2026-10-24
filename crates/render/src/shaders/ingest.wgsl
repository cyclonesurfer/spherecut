// A 9- to 16-bit plane, uploaded as integers, into its canonical f16
// texture as code / (2^b − 1) (DECISIONS D31). P010's bits are the high
// ones, so its codes shift down by 6; LSB-aligned formats shift by 0.
// A code past 2^b − 1 (stray high bits) is clamped.

struct Ingest {
    shift: u32,
    max_code: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var scratch: texture_2d<u32>;
@group(0) @binding(1) var<uniform> ing: Ingest;

@fragment
fn fs_ingest(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(scratch, vec2<u32>(frag.xy), 0);
    let v = min(c >> vec4<u32>(ing.shift), vec4<u32>(ing.max_code));
    return vec4<f32>(v) / f32(ing.max_code);
}
