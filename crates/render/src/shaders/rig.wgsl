
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
    // term reads it, and the band's centre c_b = 90° + δ_b (Rig::band_centre,
    // F025).
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

// The feather's least half width, where the margin or the frame leaves
// less room than h past the band's centre: 0.5° (sphere::rig's
// MIN_FEATHER).
const MIN_FEATHER: f32 = 0.008726646259971648;

// The directions through an output pixel's centre and half a pixel either
// side of it, which the footprint's central differences take
// (reference.rs's Rays).
struct Rays {
    d: vec3<f32>,
    right: vec3<f32>,
    left: vec3<f32>,
    down: vec3<f32>,
    up: vec3<f32>,
};

// Where a lens's model puts a lens-frame direction in its slot, and
// whether the model images the direction at all.
struct Texel {
    px: vec2<f32>,
    ok: bool,
};

// One lens's sample for an output pixel (reference.rs's Footprint): the
// texel at the pixel's centre, how it changes across the pixel to the
// right and down, whether the model images all five rays, and θ of the
// centre's ray, off the lens's axis.
struct Foot {
    texel: vec2<f32>,
    across: mat2x2<f32>,
    ok: bool,
    theta: f32,
};

// What the distortion adds to m R at the plane point m: the tangential
// terms with their r² extensions, and the thin prism (Canonical::rest).
fn rest(l: Lens, m: vec2<f32>) -> vec2<f32> {
    let r2 = dot(m, m);
    let p1 = l.tan.z + l.ext.x * r2;
    let p2 = l.tan.w + l.ext.y * r2;
    return vec2<f32>(
        2.0 * p1 * m.x * m.y + p2 * (r2 + 2.0 * m.x * m.x) + r2 * (l.ext.z + l.ext.w * r2),
        2.0 * p2 * m.x * m.y + p1 * (r2 + 2.0 * m.y * m.y) + r2 * (l.prism.x + l.prism.y * r2),
    );
}

// Unified::project: m = v.xy / (v.z + ξ), imaged where Unified::images
// holds (z ≥ −1/ξ for ξ > 1, else z + ξ > 0), then m R(r²) + rest.
fn mei(l: Lens, v: vec3<f32>) -> Texel {
    let n = length(v);
    let u = v / n;
    let xi = l.tan.y;
    let images = n > 0.0 && select(u.z + xi > 0.0, u.z >= -1.0 / xi, xi > 1.0);
    let m = u.xy / (u.z + xi);
    let r2 = dot(m, m);
    let k = l.radial;
    let radial = 1.0 + r2 * (k.x + r2 * (k.y + r2 * (k.z + r2 * (k.w + r2 * l.tan.x))));
    let md = m * radial + rest(l, m);
    return Texel(l.focal.xy * md + l.focal.zw, images);
}

// KbLens::project: θ = atan2(|v.xy|, v.z), never acos; θ_d by the
// polynomial in θ; no image on the back axis, below θ_d = 0 or where θ_d
// leaves its base's image; the base's radius along (cos φ, sin φ), 0 on
// the axis; then + rest.
fn kb(l: Lens, v: vec3<f32>) -> Texel {
    let n = length(v);
    let u = v / n;
    let rho = length(u.xy);
    let theta = atan2(rho, u.z);
    let t2 = theta * theta;
    let k = l.radial;
    let theta_d = theta * (1.0 + t2 * (k.x + t2 * (k.y + t2 * (k.z + t2 * (k.w + t2 * l.tan.x)))));
    var r = theta_d;
    var within = true;
    switch u32(l.prism.w + 0.5) {
        case 1u: {
            r = 2.0 * sin(theta_d / 2.0);
            within = theta_d <= PI;
        }
        case 2u: {
            r = 2.0 * tan(theta_d / 2.0);
            within = theta_d < PI;
        }
        case 3u: {
            r = sin(theta_d);
            within = theta_d <= PI / 2.0;
        }
        default: {}
    }
    let images = n > 0.0 && !(rho == 0.0 && u.z < 0.0) && theta_d >= 0.0 && within;
    let m = select(vec2<f32>(0.0), u.xy * (r / rho), rho > 0.0);
    let md = m + rest(l, m);
    return Texel(l.focal.xy * md + l.focal.zw, images);
}

// The rig's model, one for both lenses.
fn lens_texel(l: Lens, v: vec3<f32>) -> Texel {
    if rig.model == 1u {
        return kb(l, v);
    }
    return mei(l, v);
}

// Rig::ray: lens_from_rig · normalize(d − q (c_b − o)).
fn lens_ray(l: Lens, d: vec3<f32>) -> vec3<f32> {
    let w = normalize(normalize(d) - rig.q * l.centre.xyz);
    return vec3<f32>(dot(l.rot[0].xyz, w), dot(l.rot[1].xyz, w), dot(l.rot[2].xyz, w));
}

// θ of a lens-frame direction, off its axis (sphere::rig's off_axis):
// atan2(|v.xy|, v.z), then one Newton step, θ + sin(θ_v − θ), on the
// driver's atan2, which may be a polynomial (lavapipe's misses by 1e-6
// rad where the band ends, 5° off 90°, and up to 3e-6) where the weights
// need θ to f32's own precision.
fn off_axis(v: vec3<f32>) -> f32 {
    let rho = length(v.xy);
    let t = atan2(rho, v.z);
    return t + (rho * cos(t) - v.z * sin(t)) / length(vec2<f32>(rho, v.z));
}

// Footprint::of, with θ beside it: a lens with no image for any of the
// five rays has no footprint, and covers nothing.
fn foot_of(l: Lens, rays: Rays) -> Foot {
    let v = lens_ray(l, rays.d);
    let c = lens_texel(l, v);
    let right = lens_texel(l, lens_ray(l, rays.right));
    let left = lens_texel(l, lens_ray(l, rays.left));
    let down = lens_texel(l, lens_ray(l, rays.down));
    let up = lens_texel(l, lens_ray(l, rays.up));
    return Foot(
        c.px,
        mat2x2<f32>(right.px - left.px, down.px - up.px),
        c.ok && right.ok && left.ok && down.ok && up.ok,
        off_axis(v),
    );
}

// margin(λ) in texels: margin_texels · 2^λ, λ clamped at 0 (Rig's
// margin_px; 10.3's text doesn't clamp).
fn margin_px(lambda: f32) -> f32 {
    return rig.margin * exp2(max(lambda, 0.0));
}

// Rig::covers at the footprint's LOD: θ at most θ_max − margin(λ) / texels
// per radian, compared in angle (10.3's cosine form covers past a
// negative bound), and the texel in [margin, side − margin] on both axes
// against the rig's side, never the slot's size.
fn covers(l: Lens, f: Foot, lambda: f32) -> bool {
    let margin = margin_px(lambda);
    let in_angle = f.theta <= l.centre.w - margin / l.prism.z;
    let side = l.frame.z;
    let in_frame = min(f.texel.x, f.texel.y) >= margin && max(f.texel.x, f.texel.y) <= side - margin;
    return f.ok && in_angle && in_frame;
}

// Rig::feather_at: h_b = min(h, max(room_b, 0.5°)), not below 0, with
// room_b = min(θ_max, θ_frame) − margin(λ) / band texels per radian − c_b.
fn feather_at(l: Lens, lambda: f32) -> f32 {
    let room = min(l.centre.w, l.frame.x) - margin_px(lambda) / l.frame.y - l.frame.w;
    return max(min(rig.feather, max(room, MIN_FEATHER)), 0.0);
}

// 1 − smoothstep(c − h, c + h, θ), Hermite's, as sphere::rig's: WGSL's
// smoothstep is undefined when its edges meet, so h ≤ 0 is a step, ½ at
// θ = c. Written as the step from the band's far end, u² (3 − 2u) with
// u = (c + h − θ) / 2h, which is the same function and keeps a small
// weight's digits that 1 − a value near 1 would lose.
fn feathered(theta: f32, c: f32, h: f32) -> f32 {
    let lo = c - h;
    let hi = c + h;
    if hi <= lo {
        if theta < lo {
            return 1.0;
        }
        if theta == lo {
            return 0.5;
        }
        return 0.0;
    }
    let u = clamp((hi - theta) / (hi - lo), 0.0, 1.0);
    return u * u * (3.0 - 2.0 * u);
}

// Rig::weights (F025): each covering lens's feathered weight about its
// band's centre c_b, normalised where they sum above 0; else the covering
// lens nearer its band's centre, by θ_b − c_b, takes 1, ½ each where both
// are equally near, one covering lens takes 1, and (0, 0) only where
// neither covers.
fn weigh(f0: Foot, f1: Foot, cov: vec2<bool>, lambda: vec2<f32>) -> vec2<f32> {
    let l0 = rig.lens[0];
    let l1 = rig.lens[1];
    let w = vec2<f32>(
        select(0.0, feathered(f0.theta, l0.frame.w, feather_at(l0, lambda.x)), cov.x),
        select(0.0, feathered(f1.theta, l1.frame.w, feather_at(l1, lambda.y)), cov.y),
    );
    let sum = w.x + w.y;
    if sum > 0.0 {
        return w / sum;
    }
    if all(cov) {
        let past = vec2<f32>(f0.theta - l0.frame.w, f1.theta - l1.frame.w);
        if past.x < past.y {
            return vec2<f32>(1.0, 0.0);
        }
        if past.y < past.x {
            return vec2<f32>(0.0, 1.0);
        }
        return vec2<f32>(0.5);
    }
    return select(vec2<f32>(0.0), vec2<f32>(1.0), cov);
}

// A lens's linear light at unit gain where its footprint samples its
// slot (Source::lens_sampled, then Source::light): the texel as a uv over
// the slot's luma size, chroma by the slot's chroma transform, the taps
// as fs_project takes them (the LOD log2 of the footprint's longer side),
// the slot's matrix and its EOTF.
fn lens_light(
    s: Source,
    p0: texture_2d<f32>,
    p1: texture_2d<f32>,
    p2: texture_2d<f32>,
    l: Lens,
    f: Foot,
) -> vec3<f32> {
    let uv = f.texel / l.slot.xy;
    let g = mat2x2<f32>(f.across[0] / l.slot.xy, f.across[1] / l.slot.xy);
    let cuv = uv * s.chroma_scale + s.chroma_offset;
    let gc = mat2x2<f32>(g[0] * s.chroma_scale, g[1] * s.chroma_scale);
    let y = sample_plane(p0, uv, g, s.size0, s.levels.x);
    let c1 = sample_plane(p1, cuv, gc, s.size1, s.levels.y);
    let c2 = sample_plane(p2, cuv, gc, s.size1, s.levels.y);
    let v = vec4<f32>(y.r, c1.r, select(c2.r, c1.g, s.kind == 0u), 1.0);
    let rgb = clamp(vec3<f32>(dot(s.m0, v), dot(s.m1, v), dot(s.m2, v)), vec3<f32>(0.0), vec3<f32>(1.0));
    return select(srgb_eotf(rgb), rgb, s.transfer == 1u);
}

// lod_map's three numbers for a lens's footprint (rig_lod_map): luma's
// LOD and chroma's as sampled, then luma's unclamped, each the nearest
// f16.
fn lens_lods(s: Source, l: Lens, f: Foot) -> vec3<f32> {
    let g = mat2x2<f32>(f.across[0] / l.slot.xy, f.across[1] / l.slot.xy);
    let gc = mat2x2<f32>(g[0] * s.chroma_scale, g[1] * s.chroma_scale);
    let ly = lod(f.across, vec2<f32>(1.0));
    let lc = lod(gc, s.size1);
    return round_f16(vec4<f32>(
        clamp(ly, 0.0, s.levels.x - 1.0),
        clamp(lc, 0.0, s.levels.y - 1.0),
        ly,
        1.0,
    )).xyz;
}

// The stitch through the view (reference::render_rig as built): each
// block's footprint, mask and weight at its own LOD, the lenses' light
// with their gains blended in linear light, then the view's dim and f16,
// as fs_project ends. DEBUG_MODE 1 samples both lenses outside any branch
// (textureSample's implicit derivatives need uniform control flow); 2
// takes level 0's taps, the mask and the weights at the footprint's LOD;
// 3 is rig_lod_map's map; 4 and 5 are blocks 0 and 1 alone, ending as the
// stitch does; 6 is (w0, 0, w1) and 7 |g0 lin0 − g1 lin1| × 8 where both
// cover, unclamped, each the nearest f16 with no dim. A fisheye view past
// θ = π is black in every mode.
@fragment
fn fs_rig(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = frag.xy;
    let r = ray_at(px);
    let rays = Rays(
        r.d,
        dir_at(px + vec2<f32>(0.5, 0.0)),
        dir_at(px - vec2<f32>(0.5, 0.0)),
        dir_at(px + vec2<f32>(0.0, 0.5)),
        dir_at(px - vec2<f32>(0.0, 0.5)),
    );
    let l0 = rig.lens[0];
    let l1 = rig.lens[1];
    let f0 = foot_of(l0, rays);
    let f1 = foot_of(l1, rays);
    let lambda = vec2<f32>(lod(f0.across, vec2<f32>(1.0)), lod(f1.across, vec2<f32>(1.0)));
    let cov = vec2<bool>(covers(l0, f0, lambda.x), covers(l1, f1, lambda.y));
    let w = weigh(f0, f1, cov, lambda);
    // What the mode shows of each lens: a lens it doesn't show isn't
    // sampled.
    var shown = w > vec2<f32>(0.0);
    if DEBUG_MODE == 4u {
        shown = vec2<bool>(cov.x, false);
    } else if DEBUG_MODE == 5u {
        shown = vec2<bool>(false, cov.y);
    } else if DEBUG_MODE == 7u {
        shown = vec2<bool>(all(cov));
    } else if DEBUG_MODE == 3u || DEBUG_MODE == 6u {
        shown = vec2<bool>(false);
    }
    var light0 = vec3<f32>(0.0);
    var light1 = vec3<f32>(0.0);
    if DEBUG_MODE == 1u {
        light0 = lens_light(src, plane0, plane1, plane2, l0, f0);
        light1 = lens_light(src_b, plane0_b, plane1_b, plane2_b, l1, f1);
    } else {
        if shown.x {
            light0 = lens_light(src, plane0, plane1, plane2, l0, f0);
        }
        if shown.y {
            light1 = lens_light(src_b, plane0_b, plane1_b, plane2_b, l1, f1);
        }
    }
    // Each lens's gain, in linear light.
    let lit0 = light0 * l0.gain.xyz;
    let lit1 = light1 * l1.gain.xyz;
    let dim = dim_at(px);
    let sum = w.x + w.y;
    var out = vec3<f32>(0.0);
    if DEBUG_MODE == 3u {
        if sum > 0.0 {
            out = select(lens_lods(src, l0, f0), lens_lods(src_b, l1, f1), w.y > w.x);
        }
    } else if DEBUG_MODE == 4u {
        out = select(vec3<f32>(0.0), round_f16(vec4<f32>(lit0 * dim, 1.0)).xyz, cov.x);
    } else if DEBUG_MODE == 5u {
        out = select(vec3<f32>(0.0), round_f16(vec4<f32>(lit1 * dim, 1.0)).xyz, cov.y);
    } else if DEBUG_MODE == 6u {
        out = round_f16(vec4<f32>(w.x, 0.0, w.y, 1.0)).xyz;
    } else if DEBUG_MODE == 7u {
        out = select(vec3<f32>(0.0), round_f16(vec4<f32>(abs(lit0 - lit1) * 8.0, 1.0)).xyz, all(cov));
    } else if sum > 0.0 {
        // The blend before f16, a lens of weight 0 adding nothing.
        let blend = select(vec3<f32>(0.0), w.x * lit0, w.x > 0.0)
            + select(vec3<f32>(0.0), w.y * lit1, w.y > 0.0);
        out = round_f16(vec4<f32>(blend / sum * dim, 1.0)).xyz;
    }
    // The poles' caps are off until M3.20; pack never makes a model 2, so
    // their reads stay in the pipeline's interface, unshown.
    let caps_read = caps.caps.x + textureSampleLevel(cap, samp, vec2<f32>(0.5), 0.0).a;
    out = select(out, vec3<f32>(caps_read), rig.model == 2u);
    // A fisheye past θ = π would show the sphere twice.
    return vec4<f32>(select(vec3<f32>(0.0), out, r.theta <= PI + 1e-4), 1.0);
}

// StripSpec::dir at the strip's point px: φ = 2π · x / width, ε = k.x −
// 2 k.x · y / height, with k.x half the band in radians, and
// d = (cos ε cos φ, cos ε sin φ, −sin ε) in the rig frame.
fn strip_dir(px: vec2<f32>) -> vec3<f32> {
    let phi = TAU * px.x / view.size.x;
    let eps = view.k.x - 2.0 * view.k.x * px.y / view.size.y;
    return vec3<f32>(cos(eps) * cos(phi), cos(eps) * sin(phi), -sin(eps));
}

// The strip pass (out_kind 2, which fs_project never reads): pixel (i, j)
// looks along StripSpec::dir at (i + ½, j + ½), and writes block
// DEBUG_MODE − 4's linear light at unit gain, the nearest f16, at the
// strip pixel's own footprint from its four ±½ px rays, as strips_of
// samples it. No mask, gain or dim: a sample's validity is
// strip_coverage's, and where the lens has no footprint it writes 0.
@fragment
fn fs_rig_strip(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = frag.xy;
    let rays = Rays(
        strip_dir(px),
        strip_dir(px + vec2<f32>(0.5, 0.0)),
        strip_dir(px - vec2<f32>(0.5, 0.0)),
        strip_dir(px + vec2<f32>(0.0, 0.5)),
        strip_dir(px - vec2<f32>(0.0, 0.5)),
    );
    var light = vec3<f32>(0.0);
    if DEBUG_MODE == 5u {
        let f = foot_of(rig.lens[1], rays);
        if f.ok {
            light = lens_light(src_b, plane0_b, plane1_b, plane2_b, rig.lens[1], f);
        }
    } else {
        let f = foot_of(rig.lens[0], rays);
        if f.ok {
            light = lens_light(src, plane0, plane1, plane2, rig.lens[0], f);
        }
    }
    return round_f16(vec4<f32>(light, 1.0));
}
