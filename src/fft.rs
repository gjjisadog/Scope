use rustfft::{num_complex::Complex, FftPlanner};

#[derive(Clone, Debug, Default)]
pub struct HarmonicRow {
    pub order: usize,
    pub frequency_hz: f64,
    pub amplitude: f32,
    pub relative_db: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FftResult {
    pub channel_name: String,
    pub sample_count: usize,
    pub sample_rate_hz: f64,
    pub fundamental_hz: f64,
    pub thd_percent: f32,
    pub spectrum: Vec<[f64; 2]>,
    pub harmonics: Vec<HarmonicRow>,
}

pub fn analyze(
    channel_name: String,
    samples: &[f32],
    sample_rate_hz: f64,
    harmonic_count: usize,
) -> Option<FftResult> {
    if samples.len() < 16 || sample_rate_hz <= 0.0 {
        return None;
    }

    let len = samples.len().next_power_of_two();
    let mean = samples.iter().copied().sum::<f32>() / samples.len() as f32;
    let mut buffer = vec![Complex::new(0.0_f32, 0.0_f32); len];
    let denom = (samples.len() - 1).max(1) as f32;

    for (index, sample) in samples.iter().enumerate() {
        let window = 0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / denom).cos();
        buffer[index] = Complex::new((*sample - mean) * window, 0.0);
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(len);
    fft.process(&mut buffer);

    let half = len / 2;
    let scale = 2.0 / samples.len() as f32;
    let mut magnitudes = Vec::with_capacity(half.saturating_sub(1));
    let mut spectrum = Vec::with_capacity(half.saturating_sub(1));
    for bin in 1..half {
        let frequency = bin as f64 * sample_rate_hz / len as f64;
        let amplitude = buffer[bin].norm() * scale;
        magnitudes.push(amplitude);
        spectrum.push([frequency, amplitude as f64]);
    }

    let (fundamental_index, fundamental_amp) = magnitudes
        .iter()
        .copied()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;
    let fundamental_bin = fundamental_index + 1;
    let fundamental_hz = fundamental_bin as f64 * sample_rate_hz / len as f64;

    let mut harmonics = Vec::new();
    let mut harmonic_power = 0.0_f64;
    for order in 1..=harmonic_count {
        let bin = fundamental_bin * order;
        if bin >= half {
            break;
        }
        let amplitude = buffer[bin].norm() * scale;
        if order > 1 {
            harmonic_power += (amplitude as f64).powi(2);
        }
        let relative_db = if fundamental_amp > 0.0 && amplitude > 0.0 {
            20.0 * (amplitude / fundamental_amp).log10()
        } else {
            f32::NEG_INFINITY
        };
        harmonics.push(HarmonicRow {
            order,
            frequency_hz: bin as f64 * sample_rate_hz / len as f64,
            amplitude,
            relative_db,
        });
    }

    let thd_percent = if fundamental_amp > 0.0 {
        (harmonic_power.sqrt() / fundamental_amp as f64 * 100.0) as f32
    } else {
        0.0
    };

    Some(FftResult {
        channel_name,
        sample_count: samples.len(),
        sample_rate_hz,
        fundamental_hz,
        thd_percent,
        spectrum,
        harmonics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_known_sine_frequency() {
        let sample_rate = 10_000.0;
        let frequency = 1_000.0;
        let samples = (0..4096)
            .map(|i| (std::f64::consts::TAU * frequency * i as f64 / sample_rate).sin() as f32)
            .collect::<Vec<_>>();
        let result = analyze("CH1".to_owned(), &samples, sample_rate, 5).unwrap();
        assert!((result.fundamental_hz - frequency).abs() < 25.0);
    }
}

