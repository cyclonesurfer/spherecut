// The projection pass (M1 plan section 4.3, DECISIONS D31): for each
// output pixel, a ray from the view's lens, rotated into the world, looked
// up in an equirect source's Y'CbCr planes with an LOD of our own, and
// converted to linear RGB. spherecut_render::reference does the same in
// f64 on the CPU.

// 0: isotropic, textureSampleLevel at our λ (goldens, the CPU check,
// `spherecut frame`); 1: anisotropic, textureSampleGrad with our
// gradients and a 16× sampler (the GUI).
override FILTER: u32 = 0u;
// 0: normal; 1: textureSample with the hardware's implicit derivatives;
// 2: level 0 only; 3: the LOD used, in red (luma) and green (chroma), and
// luma's unclamped λ in blue. 1 and 2 are the goldens' negative controls.
override DEBUG_MODE: u32 = 0u;

struct View {
    // View to world, the columns of the rotation (xyz).
    rot0: vec4<f32>,
    rot1: vec4<f32>,
    rot2: vec4<f32>,
    // The view's size in pixels.
    size: vec2<f32>,
    // θ(ρ) of `from` and `to`: tan(half) for rectilinear, half for
    // fisheye, tan(half / 2) for stereographic.
    k: vec2<f32>,
    // 0 rectilinear, 1 fisheye (equidistant), 2 stereographic.
    proj: vec2<u32>,
    morph: f32,
    _pad: f32,
};

struct Source {
    // R'G'B' = (dot(m0, s), dot(m1, s), dot(m2, s)) for s = (Y, Cb, Cr, 1)
    // (or G, B, R), each sample as code / (2^b − 1): range and matrix
    // folded in.
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // The sphere-uv rectangle the picture covers; outside it is black.
    cover: vec4<f32>,
    // Sphere uv to the luma texture's uv: the sv3d crop, then the eye.
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    // Luma texture uv to chroma texture uv: the subsampling of an odd
    // size, and the chroma siting.
    chroma_scale: vec2<f32>,
    chroma_offset: vec2<f32>,
    // Level-0 sizes of the luma and chroma textures.
    size0: vec2<f32>,
    size1: vec2<f32>,
    // Their mip counts.
    levels: vec2<f32>,
    // 0 interleaved chroma (NV12), 1 planar, 2 gray, 3 planar GBR.
    kind: u32,
    // 0 the sRGB EOTF (SDR), 1 the values are linear already.
    transfer: u32,
};

@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var<uniform> src: Source;
@group(1) @binding(1) var plane0: texture_2d<f32>;
@group(1) @binding(2) var plane1: texture_2d<f32>;
@group(1) @binding(3) var plane2: texture_2d<f32>;
// U repeats (or clamps, for a crop), V clamps.
@group(2) @binding(0) var samp: sampler;

fn theta_of(kind: u32, rho: f32, k: f32) -> f32 {
    switch kind {
        case 0u: { return atan(rho * k); }
        case 1u: { return rho * k; }
        default: { return 2.0 * atan(rho * k); }
    }
}

struct Ray {
    d: vec3<f32>,
    theta: f32,
};

// sphere::pixel::to_lens, then Lens::unproject with the morph, then the
// view's rotation.
fn ray_at(px: vec2<f32>) -> Ray {
    let p = vec2<f32>(
        2.0 * px.x / view.size.x - 1.0,
        (1.0 - 2.0 * px.y / view.size.y) * view.size.y / view.size.x,
    );
    let rho = length(p);
    let theta = mix(
        theta_of(view.proj.x, rho, view.k.x),
        theta_of(view.proj.y, rho, view.k.y),
        view.morph,
    );
    let a = select(vec2<f32>(0.0), p / max(rho, 1e-30), rho > 1e-12);
    let rot = mat3x3<f32>(view.rot0.xyz, view.rot1.xyz, view.rot2.xyz);
    let d = normalize(rot * vec3<f32>(sin(theta) * a, -cos(theta)));
    return Ray(d, theta);
}

fn dir_at(px: vec2<f32>) -> vec3<f32> {
    return ray_at(px).d;
}

// sphere::equirect::from_dir, with u in [0, 1] (the sampler wraps it) and
// the latitude as atan2, which keeps its precision at the poles.
fn uv_of(d: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(
        0.5 + atan2(d.x, -d.z) / TAU,
        0.5 - atan2(d.y, length(d.xz)) / PI,
    );
}

// sphere::equirect::uv_gradient: continuous across the seam, bounded at
// the poles.
fn uv_grad(d: vec3<f32>, dd: vec3<f32>) -> vec2<f32> {
    let r2 = d.x * d.x + d.z * d.z;
    let east = -d.z * dd.x + d.x * dd.z;
    let side2 = dd.x * dd.x + dd.z * dd.z;
    let ring = select(-0.5, 0.5, east >= 0.0);
    let du = select(clamp(east / (r2 * TAU), -0.5, 0.5), ring, side2 >= 4.0 * r2);
    let r = sqrt(max(max(r2, side2 / 4.0), 1e-24));
    return vec2<f32>(du, -dd.y / (PI * r));
}

// log2 of the longer side of a pixel's footprint, in texels.
fn lod(g: mat2x2<f32>, size: vec2<f32>) -> f32 {
    let a = length(g[0] * size);
    let b = length(g[1] * size);
    return log2(max(max(a, b), 1e-30));
}

fn sample_plane(t: texture_2d<f32>, uv: vec2<f32>, g: mat2x2<f32>, size: vec2<f32>, levels: f32) -> vec4<f32> {
    if DEBUG_MODE == 1u {
        return textureSample(t, samp, uv);
    }
    if DEBUG_MODE == 2u {
        return textureSampleLevel(t, samp, uv, 0.0);
    }
    if FILTER == 1u {
        return textureSampleGrad(t, samp, uv, g[0], g[1]);
    }
    return textureSampleLevel(t, samp, uv, clamp(lod(g, size), 0.0, levels - 1.0));
}

fn srgb_eotf(v: vec3<f32>) -> vec3<f32> {
    let lo = v / 12.92;
    let hi = pow((v + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, v <= vec3<f32>(0.04045));
}

@fragment
fn fs_project(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let r = ray_at(frag.xy);
    let d = r.d;
    // Central differences of the direction over the pixel: no derivative
    // builtins, so nothing depends on 2×2 quads, and the CPU reference
    // computes the same.
    let gx = uv_grad(d, dir_at(frag.xy + vec2<f32>(0.5, 0.0)) - dir_at(frag.xy - vec2<f32>(0.5, 0.0)));
    let gy = uv_grad(d, dir_at(frag.xy + vec2<f32>(0.0, 0.5)) - dir_at(frag.xy - vec2<f32>(0.0, 0.5)));
    let suv = uv_of(d);
    let uv = suv * src.uv_scale + src.uv_offset;
    let g = mat2x2<f32>(gx * src.uv_scale, gy * src.uv_scale);
    let cuv = uv * src.chroma_scale + src.chroma_offset;
    let gc = mat2x2<f32>(g[0] * src.chroma_scale, g[1] * src.chroma_scale);
    let y = sample_plane(plane0, uv, g, src.size0, src.levels.x);
    let c1 = sample_plane(plane1, cuv, gc, src.size1, src.levels.y);
    let c2 = sample_plane(plane2, cuv, gc, src.size1, src.levels.y);
    // The target is f16: what is written is rounded to the nearest half
    // here, so that the store is exact on any driver.
    if DEBUG_MODE == 3u {
        let ly = lod(g, src.size0);
        let lc = lod(gc, src.size1);
        return round_f16(vec4<f32>(
            clamp(ly, 0.0, src.levels.x - 1.0),
            clamp(lc, 0.0, src.levels.y - 1.0),
            ly,
            1.0,
        ));
    }
    let s = vec4<f32>(y.r, c1.r, select(c2.r, c1.g, src.kind == 0u), 1.0);
    let rgb = clamp(vec3<f32>(dot(src.m0, s), dot(src.m1, s), dot(src.m2, s)), vec3<f32>(0.0), vec3<f32>(1.0));
    let lin = select(srgb_eotf(rgb), rgb, src.transfer == 1u);
    // A fisheye past θ = π would show the sphere twice.
    let inside = r.theta <= PI + 1e-4 && all(suv >= src.cover.xy) && all(suv <= src.cover.zw);
    return round_f16(vec4<f32>(select(vec3<f32>(0.0), lin, inside), 1.0));
}
