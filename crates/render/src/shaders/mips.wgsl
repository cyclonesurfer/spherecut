// One mip level from the one above it, weighting each source texel by the
// area it shares with the destination texel (DECISIONS D31). Sizes halve
// as wgpu sizes mips, max(1, size >> 1), so an odd size's destination
// texel covers parts of three source texels and comes out exactly. The
// arithmetic is in integers: positions are scaled by the destination
// size, so destination texel j spans [j·S, (j + 1)·S) and source texel i
// spans [i·D, (i + 1)·D).

// 1 for an f16 target (R16Float, Rg16Float): the mean is rounded to the
// nearest half in the shader, so that the store is exact on any driver.
// 0 for unorm8, which lavapipe rounds to nearest (Vulkan asks it to).
override F16_TARGET: u32 = 0u;

@group(0) @binding(0) var src: texture_2d<f32>;

@fragment
fn fs_mip(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let ss = textureDimensions(src);
    let ds = max(ss / 2u, vec2<u32>(1u));
    let j = vec2<u32>(frag.xy);
    let lo = j * ss;
    let hi = lo + ss;
    let first = lo / ds;
    let last = (hi - 1u) / ds;
    var acc = vec4<f32>(0.0);
    for (var y = first.y; y <= last.y; y++) {
        let wy = f32(min(hi.y, (y + 1u) * ds.y) - max(lo.y, y * ds.y)) / f32(ss.y);
        for (var x = first.x; x <= last.x; x++) {
            let wx = f32(min(hi.x, (x + 1u) * ds.x) - max(lo.x, x * ds.x)) / f32(ss.x);
            acc += wx * wy * textureLoad(src, vec2<u32>(x, y), 0);
        }
    }
    if F16_TARGET == 1u {
        return round_f16(acc);
    }
    return acc;
}
