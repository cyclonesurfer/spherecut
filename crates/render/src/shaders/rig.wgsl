
// A lens pair stitched (M3 plan 10.3, DECISIONS D66): for each output
// pixel, the view's ray, carried to each lens through the parallax at the
// stitch distance, projected through that lens's model into its slot,
// sampled as fs_project samples, and the two blended by the feather in
// linear light. And the strip pass, which unwraps one lens about the seam
// for the seam signature (amendment 5). spherecut_render::reference's
// render_rig does the same in f64 on the CPU; spherecut_render::rig::pack
// fills these structs from a sphere::rig::Rig.

// One calibration block's lens (render::rig::LensUniform): angles in
// radians, lengths in metres, the model in its slot's pixels.
struct Lens {
    // The rows of lens_from_rig (xyz).
    rot: array<vec4<f32>, 3>,
    // c_b − o (xyz); θ_max in w, the mask being tested in angle.
    centre: vec4<f32>,
    // fx, fy, cx, cy.
    focal: vec4<f32>,
    // k1..k4.
    radial: vec4<f32>,
    // k5, ξ, p1, p2.
    tan: vec4<f32>,
    // p1', p2', s1, s2.
    ext: vec4<f32>,
    // s3, s4, texels per radian at θ_max, the KB base (0 equidistant, 1
    // equisolid, 2 stereographic, 3 orthographic).
    prism: vec4<f32>,
    // θ_frame, the band's texels per radian, the slot's side as the frame
    // term reads it.
    frame: vec4<f32>,
    // The slot's luma size, which a texel is divided by for its uv.
    slot: vec4<f32>,
    // The frame's gain, linear RGB.
    gain: vec4<f32>,
};

struct Rig {
    // By calibration block.
    lens: array<Lens, 2>,
    // 1/D, in m⁻¹.
    q: f32,
    // The feather's half width h.
    feather: f32,
    // The mask's margin at LOD 0, in texels.
    margin: f32,
    // 0 Mei, 1 Kannala–Brandt.
    model: u32,
    // Each block's track: the draw binds the slots by it, and the shader
    // never reads it.
    tracks: vec2<u32>,
    _pad: vec2<u32>,
};

// Each pole's cap (M3.20, 18.3); all zeros, caps off, until then.
struct Caps {
    caps: vec4<f32>,
};

@group(0) @binding(1) var<uniform> rig: Rig;
@group(2) @binding(1) var<uniform> caps: Caps;
@group(2) @binding(2) var cap: texture_2d<f32>;
// Lens B, calibration block 1, in the source layout; lens A is group 1's.
@group(3) @binding(0) var<uniform> src_b: Source;
@group(3) @binding(1) var plane0_b: texture_2d<f32>;
@group(3) @binding(2) var plane1_b: texture_2d<f32>;
@group(3) @binding(3) var plane2_b: texture_2d<f32>;

// The placeholders' reads of the bindings both entry points share, so
// that each pipeline's interface is the stitch's (M3.7 unit (a) writes
// the bodies). A sum, never shown: the entry points write black.
fn rig_reads(px: vec2<f32>) -> f32 {
    let uv = px / view.size;
    let a = textureSampleLevel(plane0, samp, uv, 0.0).r
        + textureSampleLevel(plane1, samp, uv, 0.0).r
        + textureSampleLevel(plane2, samp, uv, 0.0).r
        + src.size0.x;
    let b = textureSampleLevel(plane0_b, samp, uv, 0.0).r
        + textureSampleLevel(plane1_b, samp, uv, 0.0).r
        + textureSampleLevel(plane2_b, samp, uv, 0.0).r
        + src_b.size0.x;
    let l = rig.lens[0].centre.w + rig.lens[1].gain.x;
    return a + b + l + rig.q + view.k.x + f32(view.out_kind);
}

// The stitch, through the view (DEBUG_MODE 0 to 7). Black until unit (a)
// writes it.
@fragment
fn fs_rig(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let reads = rig_reads(frag.xy) + caps.caps.x + textureSampleLevel(cap, samp, vec2<f32>(0.5), 0.0).a;
    // pack never makes a model 2, so the reads stay unshown.
    return vec4<f32>(select(vec3<f32>(0.0), vec3<f32>(reads), rig.model == 2u), 1.0);
}

// The strip pass (out_kind 2, which fs_project never reads): pixel (i, j)
// looks along StripSpec::dir at (i + ½, j + ½) in the rig frame, with k.x
// half the band in radians, and writes block DEBUG_MODE − 4's linear light
// at unit gain. Black until unit (a) writes it.
@fragment
fn fs_rig_strip(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let reads = rig_reads(frag.xy);
    return vec4<f32>(select(vec3<f32>(0.0), vec3<f32>(reads), rig.model == 2u), 1.0);
}
