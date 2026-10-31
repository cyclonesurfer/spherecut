// Motion blur (M2 plan section 7.4, amendment 2 items 17 and 18): each of
// a frame's n views is projected into the linear target and added here
// into a storage buffer of vec4<f32>, which needs no float blending or
// read-write float storage texture; then the mean goes back into the
// target as its nearest half, for the encode and YUV passes to read.

struct Sum {
    // The target's drawn part, in pixels.
    size: vec2<u32>,
    // How many views the sum holds.
    n: f32,
    _pad: f32,
};

@group(0) @binding(0) var linear: texture_2d<f32>;
@group(0) @binding(1) var<uniform> sum: Sum;
@group(0) @binding(2) var<storage, read_write> acc: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> total: array<vec4<f32>>;

// Adds the target's pixel to its sum.
@compute @workgroup_size(8, 8, 1)
fn cs_add(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= sum.size) {
        return;
    }
    let i = id.y * sum.size.x + id.x;
    acc[i] += textureLoad(linear, vec2<i32>(id.xy), 0);
}

// The mean, as the nearest half (shaders/common.wgsl's round_f16), as
// every pass that writes an f16 target returns it.
@fragment
fn fs_resolve(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<u32>(frag.xy);
    let i = p.y * sum.size.x + p.x;
    return round_f16(total[i] / sum.n);
}
