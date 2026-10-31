//! Synthetic audio: stereo tones and first-order ambiX (ACN order, SN3D
//! normalisation) with a source at a known direction or a tone a
//! channel, and the measures that read them back.

use std::f64::consts::TAU;

/// A sine tone with short fades so encoders don't see a click.
fn tone(freq: f64, sample_rate: u32, n: usize, amp: f64) -> Vec<f64> {
    let fade = (sample_rate as usize / 100).max(1);
    (0..n)
        .map(|i| {
            let env = (i.min(n - 1 - i) as f64 / fade as f64).min(1.0);
            amp * env * (TAU * freq * i as f64 / sample_rate as f64).sin()
        })
        .collect()
}

/// Mono: 440 Hz.
pub fn mono(sample_rate: u32, n: usize) -> Vec<Vec<f32>> {
    vec![
        tone(440.0, sample_rate, n, 0.25)
            .into_iter()
            .map(|v| v as f32)
            .collect(),
    ]
}

/// 5.1 (FFmpeg's default six-channel layout: FL, FR, FC, LFE, BL, BR):
/// [`stereo`]'s tones in the front pair, the other four silent.
pub fn surround(sample_rate: u32, n: usize) -> Vec<Vec<f32>> {
    let mut ch = stereo(sample_rate, n);
    ch.extend((0..4).map(|_| vec![0.0; n]));
    ch
}

/// How long each of [`beeps`]' beeps lasts.
pub const BEEP_MS: usize = 100;

/// Stereo silence of `n` samples with a 1 kHz beep of [`BEEP_MS`] at
/// amplitude 0.5 starting at each sample of `onsets`, the same in both
/// channels. Each beep fades in and out over 0.25 ms, so that its onset
/// is sharp and still makes no click.
pub fn beeps(sample_rate: u32, n: usize, onsets: &[usize]) -> Vec<Vec<f32>> {
    let mut out = vec![0.0f32; n];
    let len = sample_rate as usize * BEEP_MS / 1000;
    let fade = (sample_rate as usize / 4000).max(1);
    for &start in onsets {
        for i in 0..len.min(n.saturating_sub(start)) {
            let env = (i.min(len - 1 - i) as f64 / fade as f64).min(1.0);
            out[start + i] =
                (0.5 * env * (TAU * 1000.0 * i as f64 / sample_rate as f64).sin()) as f32;
        }
    }
    vec![out.clone(), out]
}

/// [`levels`]' sections, in seconds from its start: a tone loud in the
/// left channel and half as loud in the right, silence, a tone clipped at
/// full scale, and a quiet tone.
pub const LEVELS_LOUD: (f64, f64) = (0.0, 1.0);
pub const LEVELS_SILENT: (f64, f64) = (1.0, 2.0);
pub const LEVELS_CLIPPED: (f64, f64) = (2.0, 2.5);
pub const LEVELS_QUIET: (f64, f64) = (2.5, 3.0);

/// [`LEVELS_LOUD`]'s peaks, left and right: −6 and −12 dBFS.
pub const LEVELS_LOUD_PEAKS: [f64; 2] = [0.5, 0.25];

/// [`LEVELS_QUIET`]'s peak in both channels: −20 dBFS.
pub const LEVELS_QUIET_PEAK: f64 = 0.1;

/// Stereo, `n` samples of 1 kHz in [`levels`]' sections, for `analyze`'s
/// audio measures: [`LEVELS_LOUD`] at [`LEVELS_LOUD_PEAKS`],
/// [`LEVELS_SILENT`] all zeros, [`LEVELS_CLIPPED`] a tone of twice full
/// scale cut at ±1, and [`LEVELS_QUIET`] at [`LEVELS_QUIET_PEAK`]. Each
/// tone fades in and out over 10 ms.
pub fn levels(sample_rate: u32, n: usize) -> Vec<Vec<f32>> {
    let at = |s: f64| ((s * f64::from(sample_rate)).round() as usize).min(n);
    let mut ch = vec![vec![0.0f32; n]; 2];
    let mut put = |(a, b): (f64, f64), amp: [f64; 2], clip: bool| {
        let (a, b) = (at(a), at(b));
        for (c, amp) in ch.iter_mut().zip(amp) {
            for (i, v) in tone(1000.0, sample_rate, b - a, amp)
                .into_iter()
                .enumerate()
            {
                c[a + i] = if clip { v.clamp(-1.0, 1.0) } else { v } as f32;
            }
        }
    };
    put(LEVELS_LOUD, LEVELS_LOUD_PEAKS, false);
    put(LEVELS_CLIPPED, [2.0, 2.0], true);
    put(LEVELS_QUIET, [LEVELS_QUIET_PEAK; 2], false);
    ch
}

/// Stereo: 440 Hz left, 660 Hz right.
pub fn stereo(sample_rate: u32, n: usize) -> Vec<Vec<f32>> {
    [440.0, 660.0]
        .iter()
        .map(|&f| {
            tone(f, sample_rate, n, 0.25)
                .into_iter()
                .map(|v| v as f32)
                .collect()
        })
        .collect()
}

/// A 1 kHz source encoded into first-order ambiX. `azimuth` is
/// counter-clockwise from the front (ambiX convention: +90° is left) and
/// `elevation` is up, both in degrees. Channels: W, Y, Z, X.
pub fn ambix_source(sample_rate: u32, n: usize, azimuth: f64, elevation: f64) -> Vec<Vec<f32>> {
    let s = tone(1000.0, sample_rate, n, 0.25);
    let (az, el) = (azimuth.to_radians(), elevation.to_radians());
    let gains = [
        1.0,                 // W
        az.sin() * el.cos(), // Y
        el.sin(),            // Z
        az.cos() * el.cos(), // X
    ];
    gains
        .iter()
        .map(|g| s.iter().map(|v| (v * g) as f32).collect())
        .collect()
}

/// The tones of [`ambix_tones`], one a channel in ACN order (W, Y, Z, X).
pub const AMBIX_TONES_HZ: [f64; 4] = [300.0, 500.0, 700.0, 1100.0];

/// First-order ambiX with a different tone in each channel
/// ([`AMBIX_TONES_HZ`]), so that a channel that comes back elsewhere
/// shows: not a sound field, a test of channel order.
pub fn ambix_tones(sample_rate: u32, n: usize) -> Vec<Vec<f32>> {
    AMBIX_TONES_HZ
        .iter()
        .map(|&f| {
            tone(f, sample_rate, n, 0.25)
                .into_iter()
                .map(|v| v as f32)
                .collect()
        })
        .collect()
}

/// The amplitude of the `freq` component of `samples`: the DFT at that
/// frequency (Goertzel's), scaled so that a sine of amplitude `a` lasting
/// a whole number of cycles gives `a`.
pub fn tone_level(samples: &[f32], sample_rate: u32, freq: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let w = TAU * freq / f64::from(sample_rate);
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &v) in samples.iter().enumerate() {
        let (sin, cos) = (w * i as f64).sin_cos();
        re += f64::from(v) * cos;
        im -= f64::from(v) * sin;
    }
    2.0 * (re * re + im * im).sqrt() / samples.len() as f64
}

/// Recover a source direction from first-order ambiX by the
/// intensity-vector method: `(azimuth, elevation)` in degrees.
pub fn ambix_direction(ch: &[Vec<f32>]) -> Option<(f64, f64)> {
    if ch.len() < 4 {
        return None;
    }
    let dot = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| *x as f64 * *y as f64)
            .sum::<f64>()
    };
    let w = &ch[0];
    let (y, z, x) = (dot(w, &ch[1]), dot(w, &ch[2]), dot(w, &ch[3]));
    let norm = (x * x + y * y + z * z).sqrt();
    if norm < 1e-9 {
        return None;
    }
    Some((y.atan2(x).to_degrees(), (z / norm).asin().to_degrees()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ambix_channel_has_its_own_tone() {
        let ch = ambix_tones(48_000, 48_000);
        for (i, c) in ch.iter().enumerate() {
            for (j, &f) in AMBIX_TONES_HZ.iter().enumerate() {
                let level = tone_level(c, 48_000, f);
                if i == j {
                    assert!((level - 0.25).abs() < 0.01, "channel {i}: {level}");
                } else {
                    assert!(level < 0.001, "channel {i} at {f} Hz: {level}");
                }
            }
        }
    }

    #[test]
    fn direction_round_trips() {
        for (az, el) in [
            (0.0, 0.0),
            (90.0, 0.0),
            (-135.0, 20.0),
            (45.0, 30.0),
            (30.0, 20.0),
            (120.0, -20.0),
        ] {
            let a = ambix_source(48_000, 4800, az, el);
            let (raz, rel) = ambix_direction(&a).unwrap();
            assert!(
                (raz - az).abs() < 0.01 && (rel - el).abs() < 0.01,
                "{az},{el} -> {raz},{rel}"
            );
        }
    }
}
