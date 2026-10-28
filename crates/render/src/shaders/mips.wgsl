// One mip level from the one above it, weighting each source texel by the
// area it shares with the destination texel (DECISIONS D31). Sizes halve
// as wgpu sizes mips, max(1, size >> 1), so an odd size's destination
// texel covers parts of three source texels and comes out exactly. The
// arithmetic is in integers: positions are scaled by the destination
// size, so destination texel j spans [j·S, (j + 1)·S) and source texel i
// spans [i·D, (i + 1)·D), and a source texel's weight is its overlap over
// S.

// 1 for an f16 target (R16Float, Rg16Float): the mean is rounded to the
// nearest half in the shader, so that the store is exact on any driver.
// 0 for unorm8 (R8Unorm, Rg8Unorm): the mean of the codes is worked out
// exactly and rounded to the nearest code, ties to even, and the pass
// returns k / 255, which leaves the driver's conversion nothing to round,
// as the encode pass does. Left to the driver, the mean of four codes,
// often on a half code, came out a code low on RADV, which packs a
// fragment's output to f16 toward zero before converting it, and each
// level, made from the one above, added to it.
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
    if F16_TARGET == 1u {
        var acc = vec4<f32>(0.0);
        for (var y = first.y; y <= last.y; y++) {
            let wy = f32(min(hi.y, (y + 1u) * ds.y) - max(lo.y, y * ds.y)) / f32(ss.y);
            for (var x = first.x; x <= last.x; x++) {
                let wx = f32(min(hi.x, (x + 1u) * ds.x) - max(lo.x, x * ds.x)) / f32(ss.x);
                acc += wx * wy * textureLoad(src, vec2<u32>(x, y), 0);
            }
        }
        return round_f16(acc);
    }
    // The mean is Σ ox·oy·code / (Sx·Sy), the overlaps summing to Sx
    // across and Sy down. At 16384 a side that sum reaches 255 · 2^28,
    // past a u32, so each row's h = Σ ox·code (at most 255·Sx) is split
    // into q·Sx + r as it comes: the mean is (Sx·Q + R) / (Sx·Sy), with Q
    // = Σ oy·q (at most 255·Sy) and R = Σ oy·r (under Sx·Sy); and with Q
    // = a·Sy + b, it is a + (Sx·b + R) / (Sx·Sy), whose numerator is
    // under 2·Sx·Sy, 2^29 at most. Two components, for R8 and Rg8.
    var q = vec2<u32>(0u);
    var r = vec2<u32>(0u);
    for (var y = first.y; y <= last.y; y++) {
        let oy = min(hi.y, (y + 1u) * ds.y) - max(lo.y, y * ds.y);
        var h = vec2<u32>(0u);
        for (var x = first.x; x <= last.x; x++) {
            let ox = min(hi.x, (x + 1u) * ds.x) - max(lo.x, x * ds.x);
            h += ox * vec2<u32>(round(textureLoad(src, vec2<u32>(x, y), 0).xy * 255.0));
        }
        q += oy * (h / ss.x);
        r += oy * (h % ss.x);
    }
    let area = vec2<u32>(ss.x * ss.y);
    let n = ss.x * (q % ss.y) + r;
    var k = q / ss.y + n / area;
    // To nearest, ties to even, as `reference::halve_codes` rounds (D31).
    let twice = 2u * (n % area);
    let up = (twice > area) | ((twice == area) & ((k & vec2<u32>(1u)) == vec2<u32>(1u)));
    k += select(vec2<u32>(0u), vec2<u32>(1u), up);
    return vec4<f32>(vec2<f32>(k) / 255.0, 0.0, 1.0);
}
