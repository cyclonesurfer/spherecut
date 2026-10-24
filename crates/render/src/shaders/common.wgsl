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
