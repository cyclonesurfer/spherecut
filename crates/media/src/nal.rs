//! Just enough of H.264 and HEVC's NAL units, as MP4 and Matroska store
//! them (each after its length, the stream's `avcC` or `hvcC` in its
//! extradata), to tell whether a packet's picture is a keyframe as
//! FFmpeg's decoder flags one: decoding can start there needing nothing
//! decoded before it.
//!
//! The decoder ([`crate::decode`]) reads its stream's first packet so,
//! because an edit list can hide that picture's frame: FFmpeg drops the
//! frames a stream's edit list hides, so the first frame that comes out
//! isn't the first picture decoded, and its key flag says nothing about
//! where decoding began.

/// How a stream's packets hold their NAL units: each after its length,
/// in `length_size` big-endian bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NalFormat {
    hevc: bool,
    length_size: usize,
}

/// H.264's `nal_unit_type` of an IDR picture's slice, of a slice of any
/// other picture, and of SEI.
const H264_IDR: u8 = 5;
const H264_SLICE: u8 = 1;
const H264_SEI: u8 = 6;
/// The SEI payload type of a recovery point.
const RECOVERY_POINT: usize = 6;

impl NalFormat {
    /// How the packets of a stream hold their NAL units, by FFmpeg's name
    /// for its codec and its extradata: `None` for any codec but H.264 and
    /// HEVC, and for extradata that isn't `avcC` or `hvcC` (an Annex B
    /// stream, as MPEG-TS carries, whose packets hold start codes), told
    /// apart as FFmpeg's decoders tell them.
    pub(crate) fn of(codec: &str, extradata: &[u8]) -> Option<Self> {
        let (hevc, at) = match codec {
            "h264" if extradata.len() >= 7 && extradata[0] == 1 => (false, 4),
            "hevc"
                if extradata.len() >= 23
                    && (extradata[0] != 0 || extradata[1] != 0 || extradata[2] > 1) =>
            {
                (true, 21)
            }
            _ => return None,
        };
        Some(NalFormat {
            hevc,
            length_size: usize::from(extradata[at] & 3) + 1,
        })
    }

    /// Whether `packet` holds a picture FFmpeg's decoder flags a keyframe:
    /// in HEVC, an IRAP picture of the base layer (IDR, CRA or BLA); in
    /// H.264, an IDR picture, or a reference picture with a recovery point
    /// SEI counting no frames. False for any other, and for a packet that
    /// can't be read so.
    pub(crate) fn keyframe(&self, packet: &[u8]) -> bool {
        let Some(units) = self.units(packet) else {
            return false;
        };
        let mut recovery = false;
        for nal in units {
            let Some(&head) = nal.first() else {
                return false;
            };
            if head & 0x80 != 0 {
                // forbidden_zero_bit: not a NAL unit.
                return false;
            }
            if self.hevc {
                let Some(&second) = nal.get(1) else {
                    return false;
                };
                let kind = head >> 1;
                let layer = ((head & 1) << 5) | (second >> 3);
                // The first slice of the base layer's picture says.
                if kind < 32 && layer == 0 {
                    return (16..=23).contains(&kind);
                }
                continue;
            }
            let reference = head >> 5 != 0;
            match head & 0x1f {
                H264_IDR => return true,
                // The SEI comes before the picture's slices.
                H264_SLICE => return recovery && reference,
                H264_SEI => recovery |= counts_no_frames(&rbsp(&nal[1..])),
                _ => {}
            }
        }
        false
    }

    /// The NAL units of `packet`, or `None` if a length runs past its end.
    fn units<'a>(&self, packet: &'a [u8]) -> Option<Vec<&'a [u8]>> {
        let mut units = Vec::new();
        let mut rest = packet;
        while !rest.is_empty() {
            let (len, after) = rest.split_at_checked(self.length_size)?;
            let len = len.iter().fold(0usize, |n, &b| (n << 8) | usize::from(b));
            let (nal, after) = after.split_at_checked(len)?;
            units.push(nal);
            rest = after;
        }
        Some(units)
    }
}

/// Whether an H.264 SEI's RBSP (after the NAL unit header) holds a
/// recovery point whose `recovery_frame_cnt` is 0. Its messages follow
/// one another: a payload type and a size, each as bytes of 255 and a
/// last byte less, then the payload, up to the RBSP's stop bit.
fn counts_no_frames(sei: &[u8]) -> bool {
    let mut rest = sei;
    // more_rbsp_data(): anything before the stop bit's byte.
    while !rest.is_empty() && rest != [0x80] {
        let Some((kind, after)) = sei_number(rest) else {
            return false;
        };
        let Some((size, after)) = sei_number(after) else {
            return false;
        };
        let Some((payload, after)) = after.split_at_checked(size) else {
            return false;
        };
        // recovery_frame_cnt comes first, as an Exp-Golomb code: 0 is the
        // one code that starts with a 1 bit.
        if kind == RECOVERY_POINT && payload.first().is_some_and(|b| b & 0x80 != 0) {
            return true;
        }
        rest = after;
    }
    false
}

/// An SEI message's payload type or size, and the bytes after it.
fn sei_number(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let ff = bytes.iter().take_while(|&&b| b == 0xff).count();
    let last = *bytes.get(ff)?;
    Some((255 * ff + usize::from(last), &bytes[ff + 1..]))
}

/// A NAL unit's payload without its emulation prevention bytes: a 3
/// after two zero bytes.
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;

    /// `avcC` with 4-byte lengths and no parameter sets, enough for
    /// [`NalFormat::of`].
    const AVCC: [u8; 7] = [1, 100, 0, 40, 0xff, 0xe0, 0];

    fn h264(length_size: usize) -> NalFormat {
        NalFormat {
            hevc: false,
            length_size,
        }
    }

    fn hevc() -> NalFormat {
        NalFormat {
            hevc: true,
            length_size: 4,
        }
    }

    /// `rbsp` escaped as an encoder escapes it: a 3 after two zero bytes
    /// where the next byte is 3 or less.
    fn escape(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rbsp.len());
        let mut zeros = 0;
        for &b in rbsp {
            if zeros >= 2 && b <= 3 {
                out.push(3);
                zeros = 0;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            out.push(b);
        }
        out
    }

    /// NAL units after their lengths in `length_size` bytes.
    fn packet(length_size: usize, units: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for u in units {
            out.extend_from_slice(&u.len().to_be_bytes()[8 - length_size..]);
            out.extend_from_slice(u);
        }
        out
    }

    /// An SEI message's payload type or size.
    fn number(mut n: usize) -> Vec<u8> {
        let mut out = vec![0xff; n / 255];
        n %= 255;
        out.push(n as u8);
        out
    }

    /// A recovery point payload counting `count` frames: the count as an
    /// Exp-Golomb code, then `exact_match_flag`, `broken_link_flag` and
    /// `changing_slice_group_idc`, padded to a byte.
    fn recovery_payload(count: u32) -> Vec<u8> {
        let v = u64::from(count) + 1;
        let n = 64 - v.leading_zeros();
        let mut bits = vec![false; n as usize - 1];
        bits.extend((0..n).rev().map(|i| (v >> i) & 1 == 1));
        bits.extend([true, false, false, false]);
        bits.resize(bits.len().next_multiple_of(8), false);
        bits.chunks(8)
            .map(|c| c.iter().fold(0, |b, &x| (b << 1) | u8::from(x)))
            .collect()
    }

    /// An SEI NAL unit of `messages`, each a payload type and payload.
    fn sei(messages: &[(usize, Vec<u8>)]) -> Vec<u8> {
        let mut rbsp = Vec::new();
        for (kind, payload) in messages {
            rbsp.extend(number(*kind));
            rbsp.extend(number(payload.len()));
            rbsp.extend_from_slice(payload);
        }
        rbsp.push(0x80);
        let mut nal = vec![H264_SEI];
        nal.extend(escape(&rbsp));
        nal
    }

    /// An H.264 slice NAL unit of `kind` and `nal_ref_idc` `idc`, with
    /// `body` for the rest.
    fn slice(kind: u8, idc: u8, body: &[u8]) -> Vec<u8> {
        let mut nal = vec![(idc << 5) | kind];
        nal.extend(escape(body));
        nal
    }

    #[test]
    fn avcc_and_hvcc_are_told_from_annex_b() {
        assert_eq!(NalFormat::of("h264", &AVCC), Some(h264(4)));
        let mut two = AVCC;
        two[4] = 0xfd;
        assert_eq!(NalFormat::of("h264", &two), Some(h264(2)));
        // Annex B extradata starts with a start code.
        assert_eq!(NalFormat::of("h264", &[0, 0, 0, 1, 0x67, 100, 0]), None);
        assert_eq!(NalFormat::of("h264", &[]), None);
        let mut hvcc = [0u8; 23];
        hvcc[0] = 1;
        hvcc[21] = 0x0f;
        assert_eq!(NalFormat::of("hevc", &hvcc), Some(hevc()));
        let mut annex_b = hvcc;
        annex_b[..4].copy_from_slice(&[0, 0, 0, 1]);
        assert_eq!(NalFormat::of("hevc", &annex_b), None);
        assert_eq!(NalFormat::of("hevc", &hvcc[..22]), None);
        assert_eq!(NalFormat::of("av1", &AVCC), None);
        assert_eq!(NalFormat::of("mpeg4", &AVCC), None);
    }

    #[test]
    fn h264_keyframes_are_idr_or_recover_at_once() {
        let f = h264(4);
        let idr = slice(H264_IDR, 3, &[0x88, 0x84]);
        let p = slice(H264_SLICE, 2, &[0x9a, 0x02]);
        let b = slice(H264_SLICE, 0, &[0x9e, 0x02]);
        // An access unit delimiter, SPS and PPS before the slice.
        let aud = vec![9, 0xf0];
        let sps = vec![0x67, 100, 0, 40];
        let pps = vec![0x68, 0xee];
        assert!(f.keyframe(&packet(4, &[aud.clone(), sps, pps, idr.clone()])));
        assert!(!f.keyframe(&packet(4, &[aud.clone(), p.clone()])));
        let now = sei(&[(RECOVERY_POINT, recovery_payload(0))]);
        let later = sei(&[(RECOVERY_POINT, recovery_payload(4))]);
        assert!(f.keyframe(&packet(4, &[aud.clone(), now.clone(), p.clone()])));
        assert!(!f.keyframe(&packet(4, &[aud.clone(), later, p.clone()])));
        // Not on a picture nothing refers to.
        assert!(!f.keyframe(&packet(4, &[now.clone(), b])));
        // An SEI after the slice is not this picture's.
        assert!(!f.keyframe(&packet(4, &[p.clone(), now.clone()])));
        // After another message, one with a long payload.
        let long = sei(&[(5, vec![0x11; 300]), (RECOVERY_POINT, recovery_payload(0))]);
        assert!(f.keyframe(&packet(4, &[long, p.clone()])));
        // Lengths in 2 bytes.
        assert!(h264(2).keyframe(&packet(2, &[now.clone(), p.clone()])));
        // Damage: a length past the end, or a forbidden bit.
        let mut cut = packet(4, std::slice::from_ref(&idr));
        cut[3] += 1;
        assert!(!f.keyframe(&cut));
        let mut bad = idr;
        bad[0] |= 0x80;
        assert!(!f.keyframe(&packet(4, &[bad])));
        assert!(!f.keyframe(&[]));
        let short = sei(&[(RECOVERY_POINT, vec![])]);
        assert!(!f.keyframe(&packet(4, &[short, p])));
    }

    #[test]
    fn hevc_keyframes_are_base_layer_irap_pictures() {
        let f = hevc();
        let unit =
            |kind: u8, layer: u8| vec![(kind << 1) | (layer >> 5), ((layer & 31) << 3) | 1, 0xaf];
        // VPS, SPS, PPS and a prefix SEI before the slice.
        let params = [unit(32, 0), unit(33, 0), unit(34, 0), unit(39, 0)];
        for kind in 0..=31 {
            let mut units = params.to_vec();
            units.push(unit(kind, 0));
            assert_eq!(
                f.keyframe(&packet(4, &units)),
                (16..=23).contains(&kind),
                "{kind}"
            );
        }
        // An enhancement layer's IRAP slice first: the base layer's says.
        let units = [unit(21, 1), unit(1, 0)];
        assert!(!f.keyframe(&packet(4, &units)));
        let units = [unit(1, 1), unit(19, 0)];
        assert!(f.keyframe(&packet(4, &units)));
        assert!(!f.keyframe(&packet(4, &[vec![42]])));
    }

    fn message() -> impl Strategy<Value = (usize, Vec<u8>)> {
        (0usize..600, vec(any::<u8>(), 0..600))
            .prop_filter("no recovery point", |(kind, _)| *kind != RECOVERY_POINT)
    }

    proptest! {
        #[test]
        fn rbsp_undoes_escaping(bytes in vec(prop_oneof![Just(0u8), Just(3u8), any::<u8>()], 0..200)) {
            prop_assert_eq!(rbsp(&escape(&bytes)), bytes);
        }

        #[test]
        fn a_recovery_point_is_found_among_other_messages(
            before in vec(message(), 0..4),
            after in vec(message(), 0..4),
            count in prop_oneof![Just(0u32), any::<u32>()],
            idc in 0u8..4,
            length_size in prop_oneof![Just(1usize), Just(2), Just(4)],
            body in vec(prop_oneof![Just(0u8), any::<u8>()], 1..40),
        ) {
            let mut messages = before;
            messages.push((RECOVERY_POINT, recovery_payload(count)));
            messages.extend(after);
            let units = [sei(&messages), slice(H264_SLICE, idc, &body)];
            // Lengths in 1 byte where they fit; no unit reaches 65536.
            let length_size = match units.iter().all(|u| u.len() < 256) {
                true => length_size,
                false => length_size.max(2),
            };
            let f = h264(length_size);
            prop_assert_eq!(f.keyframe(&packet(length_size, &units)), count == 0 && idc != 0);
        }

        /// Any bytes at all, in either codec: an answer, never a panic.
        #[test]
        fn any_packet_is_read_without_panicking(
            hevc in any::<bool>(),
            length_size in 1usize..=4,
            bytes in vec(any::<u8>(), 0..300),
        ) {
            let _ = NalFormat { hevc, length_size }.keyframe(&bytes);
        }
    }
}
