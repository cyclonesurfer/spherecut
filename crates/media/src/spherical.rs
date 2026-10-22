//! Spherical Video (V1 and V2) and Spatial Audio (SA3D) metadata.
//!
//! V2 (`st3d`, `sv3d` in the video sample entry) is what we write on 360
//! export and what YouTube and headsets read. V1 (an XML `uuid` box in the
//! video `trak`) is still found in files from older tools and cameras.
//! References: Google's spatial-media `docs/spherical-video-v2-rfc.md`,
//! `docs/spherical-video-rfc.md` and `docs/spatial-audio-rfc.md`.

use serde::Serialize;

use crate::mp4::{Cursor, Mp4Box, Mp4Tree};

/// The V1 spherical metadata uuid, ffcc8263-f855-4a93-8814-587a02521fdd.
pub const SPHERICAL_V1_UUID: [u8; 16] = [
    0xff, 0xcc, 0x82, 0x63, 0xf8, 0x55, 0x4a, 0x93, 0x88, 0x14, 0x58, 0x7a, 0x02, 0x52, 0x1f, 0xdd,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StereoMode {
    Mono,
    TopBottom,
    LeftRight,
    StereoCustom,
    Unknown(u8),
}

impl StereoMode {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => StereoMode::Mono,
            1 => StereoMode::TopBottom,
            2 => StereoMode::LeftRight,
            3 => StereoMode::StereoCustom,
            o => StereoMode::Unknown(o),
        }
    }
    pub fn to_u8(self) -> u8 {
        match self {
            StereoMode::Mono => 0,
            StereoMode::TopBottom => 1,
            StereoMode::LeftRight => 2,
            StereoMode::StereoCustom => 3,
            StereoMode::Unknown(v) => v,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Projection {
    /// Equirectangular. Bounds are 0.32 fixed-point fractions cropped from
    /// each edge; all zero for a full sphere.
    Equirect {
        top: u32,
        bottom: u32,
        left: u32,
        right: u32,
    },
    Cubemap {
        layout: u32,
        padding: u32,
    },
    Mesh,
    Other {
        fourcc: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SphericalV2 {
    pub stereo: Option<StereoMode>,
    pub metadata_source: Option<String>,
    /// Pose in degrees (stored as 16.16 fixed point).
    pub pose_yaw: f64,
    pub pose_pitch: f64,
    pub pose_roll: f64,
    pub projection: Option<Projection>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SphericalV1 {
    pub spherical: bool,
    pub stitched: bool,
    pub projection_type: Option<String>,
    pub stereo_mode: Option<String>,
    pub stitching_software: Option<String>,
    pub raw_xml: String,
}

/// Spatial audio (SA3D) box contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpatialAudio {
    pub version: u8,
    /// 0 = periphonic (full-sphere) ambisonics.
    pub ambisonic_type: u8,
    pub ambisonic_order: u32,
    /// 0 = ACN.
    pub channel_ordering: u8,
    /// 0 = SN3D.
    pub normalization: u8,
    pub channel_map: Vec<u32>,
}

impl SpatialAudio {
    /// Whether this is ambiX (ACN order, SN3D normalization, periphonic).
    pub fn is_ambix(&self) -> bool {
        self.ambisonic_type == 0 && self.channel_ordering == 0 && self.normalization == 0
    }
}

/// Spherical metadata found on one track.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TrackSpherical {
    pub v2: Option<SphericalV2>,
    pub v1: Option<SphericalV1>,
    pub spatial_audio: Option<SpatialAudio>,
}

impl TrackSpherical {
    pub fn is_empty(&self) -> bool {
        self.v1.is_none() && self.v2.is_none() && self.spatial_audio.is_none()
    }
}

/// Spherical metadata for every track, in file track order.
pub fn read_tracks(tree: &Mp4Tree) -> Vec<TrackSpherical> {
    tree.tracks().map(read_track).collect()
}

fn sample_entries(trak: &Mp4Box) -> impl Iterator<Item = &Mp4Box> {
    trak.path(&[b"mdia", b"minf", b"stbl", b"stsd"])
        .into_iter()
        .flat_map(|s| s.children.iter())
}

pub fn read_track(trak: &Mp4Box) -> TrackSpherical {
    let mut out = TrackSpherical::default();
    for entry in sample_entries(trak) {
        let st3d = entry.child(b"st3d").and_then(parse_st3d);
        let sv3d = entry.child(b"sv3d");
        if st3d.is_some() || sv3d.is_some() {
            let mut v2 = sv3d.map(parse_sv3d).unwrap_or(SphericalV2 {
                stereo: None,
                metadata_source: None,
                pose_yaw: 0.0,
                pose_pitch: 0.0,
                pose_roll: 0.0,
                projection: None,
            });
            v2.stereo = st3d;
            out.v2 = Some(v2);
        }
        if let Some(sa3d) = entry.child(b"SA3D").and_then(|b| parse_sa3d(&b.payload)) {
            out.spatial_audio = Some(sa3d);
        }
    }
    if let Some(u) = trak
        .children
        .iter()
        .find(|b| b.fourcc.0 == *b"uuid" && b.uuid == Some(SPHERICAL_V1_UUID))
    {
        out.v1 = Some(parse_v1(&u.payload));
    }
    out
}

fn parse_st3d(b: &Mp4Box) -> Option<StereoMode> {
    let mut c = Cursor::new(&b.payload);
    c.version_flags()?;
    Some(StereoMode::from_u8(c.u8()?))
}

fn fixed16(v: i32) -> f64 {
    v as f64 / 65536.0
}

fn parse_sv3d(b: &Mp4Box) -> SphericalV2 {
    let metadata_source = b.child(b"svhd").map(|h| {
        let mut c = Cursor::new(&h.payload);
        let _ = c.version_flags();
        c.cstr()
    });
    let mut v = SphericalV2 {
        stereo: None,
        metadata_source,
        pose_yaw: 0.0,
        pose_pitch: 0.0,
        pose_roll: 0.0,
        projection: None,
    };
    if let Some(proj) = b.child(b"proj") {
        if let Some(prhd) = proj.child(b"prhd") {
            let mut c = Cursor::new(&prhd.payload);
            if c.version_flags().is_some() {
                v.pose_yaw = c.i32().map(fixed16).unwrap_or(0.0);
                v.pose_pitch = c.i32().map(fixed16).unwrap_or(0.0);
                v.pose_roll = c.i32().map(fixed16).unwrap_or(0.0);
            }
        }
        v.projection = proj
            .children
            .iter()
            .find(|c| c.fourcc.0 != *b"prhd")
            .map(|p| {
                let mut c = Cursor::new(&p.payload);
                let _ = c.version_flags();
                match &p.fourcc.0 {
                    b"equi" => Projection::Equirect {
                        top: c.u32().unwrap_or(0),
                        bottom: c.u32().unwrap_or(0),
                        left: c.u32().unwrap_or(0),
                        right: c.u32().unwrap_or(0),
                    },
                    b"cbmp" => Projection::Cubemap {
                        layout: c.u32().unwrap_or(0),
                        padding: c.u32().unwrap_or(0),
                    },
                    b"mshp" => Projection::Mesh,
                    _ => Projection::Other {
                        fourcc: p.fourcc.to_string(),
                    },
                }
            });
    }
    v
}

pub fn parse_sa3d(payload: &[u8]) -> Option<SpatialAudio> {
    let mut c = Cursor::new(payload);
    let version = c.u8()?;
    let ambisonic_type = c.u8()?;
    let ambisonic_order = c.u32()?;
    let channel_ordering = c.u8()?;
    let normalization = c.u8()?;
    let n = c.u32()?;
    // A first-order file has 4 channels; even 7th order has 64. Reject
    // nonsense counts instead of allocating for them.
    if n > 1024 {
        return None;
    }
    let channel_map = (0..n).map(|_| c.u32()).collect::<Option<Vec<_>>>()?;
    Some(SpatialAudio {
        version,
        ambisonic_type,
        ambisonic_order,
        channel_ordering,
        normalization,
        channel_map,
    })
}

fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    // Tags may carry a namespace prefix such as `GSpherical:`.
    let open = format!("{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find('<')? + start;
    Some(xml[start..end].trim().to_string())
}

fn parse_v1(payload: &[u8]) -> SphericalV1 {
    let xml = String::from_utf8_lossy(payload).into_owned();
    let is_true = |t: &str| xml_tag(&xml, t).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    SphericalV1 {
        spherical: is_true("Spherical"),
        stitched: is_true("Stitched"),
        projection_type: xml_tag(&xml, "ProjectionType"),
        stereo_mode: xml_tag(&xml, "StereoMode"),
        stitching_software: xml_tag(&xml, "StitchingSoftware"),
        raw_xml: xml,
    }
}

/// Writers for the boxes above, used by 360 export and by the synthetic
/// test-media generator.
pub mod write {
    use super::*;

    fn bx(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(payload);
        v
    }

    fn full(t: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0];
        p.extend_from_slice(payload);
        bx(t, &p)
    }

    pub fn st3d(mode: StereoMode) -> Vec<u8> {
        full(b"st3d", &[mode.to_u8()])
    }

    /// `sv3d` for a full-sphere equirect with zero pose.
    pub fn sv3d_equirect(metadata_source: &str) -> Vec<u8> {
        let mut src = metadata_source.as_bytes().to_vec();
        src.push(0);
        let svhd = full(b"svhd", &src);
        let prhd = full(b"prhd", &[0; 12]);
        let equi = full(b"equi", &[0; 16]);
        let proj = bx(b"proj", &[prhd, equi].concat());
        bx(b"sv3d", &[svhd, proj].concat())
    }

    /// First-order ambiX (ACN/SN3D) SA3D box with an identity channel map.
    pub fn sa3d_first_order_ambix() -> Vec<u8> {
        let mut p = vec![0u8, 0];
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&[0, 0]);
        p.extend_from_slice(&4u32.to_be_bytes());
        for i in 0..4u32 {
            p.extend_from_slice(&i.to_be_bytes());
        }
        bx(b"SA3D", &p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4::tests::{bx, cat, full};
    use crate::mp4::{self};

    fn file_with(video_extra: Vec<u8>, audio_extra: Vec<u8>, trak_extra: Vec<u8>) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isom");
        let ventry = bx(b"hvc1", &cat(&[vec![0u8; 78], video_extra]));
        let vstsd = full(b"stsd", 0, 0, &cat(&[1u32.to_be_bytes().to_vec(), ventry]));
        let vtrak = bx(
            b"trak",
            &cat(&[bx(b"mdia", &bx(b"minf", &bx(b"stbl", &vstsd))), trak_extra]),
        );
        let aentry = bx(b"mp4a", &cat(&[vec![0u8; 28], audio_extra]));
        let astsd = full(b"stsd", 0, 0, &cat(&[1u32.to_be_bytes().to_vec(), aentry]));
        let atrak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &bx(b"stbl", &astsd))));
        cat(&[ftyp, bx(b"moov", &cat(&[vtrak, atrak]))])
    }

    #[test]
    fn writes_and_reads_back_v2_and_sa3d() {
        let f = file_with(
            cat(&[
                write::st3d(StereoMode::Mono),
                write::sv3d_equirect("Spherecut"),
            ]),
            write::sa3d_first_order_ambix(),
            vec![],
        );
        let tree = mp4::read(&mut std::io::Cursor::new(f)).unwrap();
        let t = read_tracks(&tree);
        assert_eq!(t.len(), 2);
        let v2 = t[0].v2.as_ref().unwrap();
        assert_eq!(v2.stereo, Some(StereoMode::Mono));
        assert_eq!(v2.metadata_source.as_deref(), Some("Spherecut"));
        assert_eq!(
            v2.projection,
            Some(Projection::Equirect {
                top: 0,
                bottom: 0,
                left: 0,
                right: 0
            })
        );
        let sa = t[1].spatial_audio.as_ref().unwrap();
        assert!(sa.is_ambix());
        assert_eq!(sa.ambisonic_order, 1);
        assert_eq!(sa.channel_map, vec![0, 1, 2, 3]);
    }

    #[test]
    fn reads_v1_xml() {
        let xml = br#"<?xml version="1.0"?><rdf:SphericalVideo xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:GSpherical="http://ns.google.com/videos/1.0/spherical/"><GSpherical:Spherical>true</GSpherical:Spherical><GSpherical:Stitched>true</GSpherical:Stitched><GSpherical:StitchingSoftware>Test</GSpherical:StitchingSoftware><GSpherical:ProjectionType>equirectangular</GSpherical:ProjectionType></rdf:SphericalVideo>"#;
        let uuid = bx(b"uuid", &cat(&[SPHERICAL_V1_UUID.to_vec(), xml.to_vec()]));
        let tree = mp4::read(&mut std::io::Cursor::new(file_with(vec![], vec![], uuid))).unwrap();
        let v1 = read_tracks(&tree)[0].v1.clone().unwrap();
        assert!(v1.spherical && v1.stitched);
        assert_eq!(v1.projection_type.as_deref(), Some("equirectangular"));
        assert_eq!(v1.stitching_software.as_deref(), Some("Test"));
    }

    #[test]
    fn garbage_sa3d_is_rejected() {
        assert!(parse_sa3d(&[0, 0, 0, 0, 0, 1, 0, 0, 0xff, 0xff, 0xff, 0xff]).is_none());
        assert!(parse_sa3d(&[]).is_none());
    }
}
