//! Pat's four calibration strings, in one place (the project review's
//! F137, M3 plan amendment 7): byte for byte as fields 5, 53, 54 and 111
//! of Pat's X5 files hold them, in all 14 of which they are identical
//! (`docs/formats/insta360.md`, "Pat's X5 samples", which lays them out
//! by token).
//!
//! Every test that needs Pat's numbers reads them from here, through
//! [`tokens`] or a parser of its own, or holds its own copy to these by a
//! drift test: `spherecut-boundary` builds synth's truths by parsing V3
//! and V6, camera's Tier 2c test compares these with the files, and the
//! copies left in synth, sphere, camera and cli each have a test that
//! fails when one of their numbers stops being these strings'.
//!
//! This module holds text, not meaning: which token is which number is
//! the format notes' §4.1, which each reader applies with its own code.

/// V1 (field 5): the lens count, then a block a lens of `radius cx cy
/// yaw pitch roll`, then the canvas's width and height and the packed
/// word.
pub const V1: &str = "2_2653.545_2701.130_2692.130_-0.198_0.211_90.280_2661.203_8069.000_2684.950_0.239_0.113_89.107_10752_5376_1137";

/// V2 (field 53): the lens count, then a block a lens of `radius cx cy
/// yaw pitch roll tx ty tz c0 c1 c2 c3 w h type`, then the packed word.
pub const V2: &str = "2_2660.372_2701.130_2692.130_-0.198_0.211_90.280_0.000000_0.000000_0.000000_1.00000000_-0.18756898_0.27067676_-0.08894466_10752_5376_113_2670.937_8069.000_2684.950_0.239_0.113_89.107_-0.004460_0.000017_-0.031920_1.00000000_-0.18880324_0.27227741_-0.08904964_10752_5376_113_132096";

/// V3 (field 54): the lens count, then a block a lens of `xi fx fy cx cy
/// yaw pitch roll tx ty tz k1 k2 k3 p1 p2 w h type`, then the packed
/// word.
pub const V3: &str = "2_2.000000_4284.620_4284.280_2701.130_2692.130_-0.198_0.211_90.280_0.000000_0.000000_0.000000_0.18632051_2.04984355_-3.24446917_-0.00143576_-0.00339240_10752_5376_113_2.000000_4291.030_4291.030_8069.000_2684.950_0.239_0.113_89.107_-0.004460_0.000017_-0.031920_0.18940836_2.07073069_-3.24103332_0.00013831_-0.00299873_10752_5376_113_197632";

/// V6 (field 111): the lens count, then a block a lens of `xi fx fy cx cy
/// yaw pitch roll tx ty tz d0 … d12 w h type`, then the packed word.
pub const V6: &str = "2_2.000000_4277.520_4277.270_2713.050_2689.210_-0.310_-0.271_90.255_0.000000_0.000000_0.000000_0.28032997_0.89045405_2.04927588_-8.05179787_0.00000000_0.00120200_-0.00259487_0.02324706_0.00533920_-0.01560208_0.00019357_-0.04580523_0.00197187_10752_5376_113_2.000000_4284.400_4284.760_8075.730_2680.350_0.120_-0.173_89.112_0.000637_0.000001_-0.031525_0.27665332_0.98522973_1.83605266_-7.99374485_0.00000000_0.00021056_-0.00147626_0.01940425_0.00331838_-0.01330665_0.00074511_-0.02859299_0.00888746_10752_5376_113_394240";

/// The numbers of a calibration string `s`, in token order: each token
/// between the `_`s, as `str::parse::<f64>` reads it, the integers (the
/// lens count, the canvas, the lens type and the packed word) among
/// them. A string of two lenses gives 16 numbers in V1, 34 in V2, 40 in
/// V3 and 56 in V6.
///
/// # Panics
///
/// If a token isn't a number: these are test strings, and a test that
/// reads one that isn't wants to stop there.
pub fn tokens(s: &str) -> Vec<f64> {
    s.split('_')
        .map(|t| {
            t.parse()
                .unwrap_or_else(|_| panic!("{t:?} in a calibration string isn't a number"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::must_fail;

    /// Each string has its generation's count of numbers for two lenses
    /// (insta360.md §4.1: 6N + 4, 16N + 2, 19N + 2 and 27N + 2), and its
    /// packed word names its generation in bits 16 and up (0, 2, 3 and
    /// 6). The control drops V3's last token, the word, as synth's
    /// `WrongTokenCount` fault does, and the count no longer holds.
    #[test]
    fn each_string_has_its_generations_tokens() {
        for (s, count, code) in [(V1, 16, 0), (V2, 34, 2), (V3, 40, 3), (V6, 56, 6)] {
            let numbers = tokens(s);
            assert_eq!(numbers.len(), count, "{s}");
            assert_eq!(numbers[0], 2.0, "the lens count of {s}");
            let word = numbers[count - 1];
            assert_eq!(word.fract(), 0.0, "{s}");
            assert_eq!((word as u32) >> 16, code, "{s}");
        }
        let short = V3.rsplit_once('_').map_or(V3, |(head, _)| head);
        must_fail("V3 without its word", tokens(short).len() == 40);
    }
}
