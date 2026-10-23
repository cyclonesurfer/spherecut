//! Synthetic audio: stereo tones and first-order ambiX (ACN order, SN3D
//! normalisation) with a source at a known direction.

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
