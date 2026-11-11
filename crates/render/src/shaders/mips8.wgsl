// The mip pass for 8-bit planes (R8Unorm, Rg8Unorm) as a compute pass
// (DECISIONS D77), after `mips.wgsl`: level n's codes from level n − 1
// (`src`), each texel `mean_codes`, as fs_mip computes it, packed into
// `dst`, a region of a buffer that a copy then puts into level n. Slots
// make these planes without RENDER_ATTACHMENT, which keeps RADV from
// compressing them (DCC), so no fragment pass can write them.
//
// A word holds four R8 texels or two Rg8 ones, the first in its low byte,
// as a copy reads them. A row of level n starts every arrayLength(&dst) /
// rows words: the caller binds exactly the rows, each padded to wgpu's
// 256 bytes (COPY_BYTES_PER_ROW_ALIGNMENT), and the words past a row's
// texels are left alone.

// 1 for R8, 2 for Rg8.
override COMPONENTS: u32 = 1u;
// 1 only for a test's control: each mean truncated to the code below it,
// one code off wherever it rounds up.
override TRUNCATE: u32 = 0u;

@group(1) @binding(0) var<storage, read_write> dst: array<u32>;

// `upload::MIP8_GROUP` dispatches groups of this size.
@compute @workgroup_size(64, 4)
fn cs_mip8(@builtin(global_invocation_id) id: vec3<u32>) {
    let ds = max(textureDimensions(src) / 2u, vec2<u32>(1u));
    let texels = 4u / COMPONENTS;
    let words = (ds.x + texels - 1u) / texels;
    if id.x >= words || id.y >= ds.y {
        return;
    }
    let pitch = arrayLength(&dst) / ds.y;
    var word = 0u;
    for (var t = 0u; t < texels; t++) {
        let x = id.x * texels + t;
        if x < ds.x {
            let k = mean_codes(vec2<u32>(x, id.y), TRUNCATE == 0u);
            let at = 8u * COMPONENTS * t;
            word |= k.x << at;
            if COMPONENTS == 2u {
                word |= k.y << (at + 8u);
            }
        }
    }
    dst[id.y * pitch + id.x] = word;
}
