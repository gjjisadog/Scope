use rustfft::{num_complex::Complex, FftPlanner};

#[derive(Clone, Debug, Default)]
pub struct HarmonicRow {
    pub order: usize,
    pub amplitude: f32,
    pub phase_deg: f32,
    pub relative_percent: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FftResult {
    pub channel_name: String,
    pub sample_count: usize,
    pub thd_percent: f32,
    pub harmonics: Vec<HarmonicRow>,
}

fn fft_buffer(samples: &[f32]) -> Vec<Complex<f32>> {
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
    buffer
}

fn phase_deg(value: Complex<f32>) -> f32 {
    value.arg().to_degrees()
}

pub fn analyze(
    channel_name: String,
    samples: &[f32],
    sample_rate_hz: f64,
    harmonic_base_hz: f64,
    harmonic_count: usize,
) -> Option<FftResult> {
    if samples.len() < 16 || sample_rate_hz <= 0.0 || harmonic_base_hz <= 0.0 {
        return None;
    }

    let mean = samples.iter().copied().sum::<f32>() / samples.len() as f32;
    let dc_amplitude = mean.abs();
    let buffer = fft_buffer(samples);
    let len = buffer.len();
    let half = len / 2;
    let scale = 2.0 / samples.len() as f32;

    let bin_for_frequency = |frequency_hz: f64| -> Option<usize> {
        let bin = (frequency_hz * len as f64 / sample_rate_hz).round() as usize;
        (bin > 0 && bin < half).then_some(bin)
    };
    let fundamental_bin = bin_for_frequency(harmonic_base_hz)?;
    let fundamental_amp = buffer[fundamental_bin].norm() * scale;

    let mut harmonics = Vec::with_capacity(harmonic_count.saturating_add(1));
    let dc_relative_percent = if fundamental_amp > 0.0 {
        dc_amplitude / fundamental_amp * 100.0
    } else {
        0.0
    };
    harmonics.push(HarmonicRow {
        order: 0,
        amplitude: dc_amplitude,
        phase_deg: f32::NAN,
        relative_percent: dc_relative_percent,
    });

    let mut harmonic_power = 0.0_f64;
    for order in 1..=harmonic_count {
        let Some(bin) = bin_for_frequency(harmonic_base_hz * order as f64) else {
            break;
        };
        let amplitude = buffer[bin].norm() * scale;
        if order > 1 {
            harmonic_power += (amplitude as f64).powi(2);
        }
        let relative_percent = if fundamental_amp > 0.0 {
            amplitude / fundamental_amp * 100.0
        } else {
            0.0
        };
        let phase_deg = phase_deg(buffer[bin]);
        harmonics.push(HarmonicRow {
            order,
            amplitude,
            phase_deg,
            relative_percent,
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
        thd_percent,
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
        let result = analyze("CH1".to_owned(), &samples, sample_rate, frequency, 5).unwrap();
        assert_eq!(result.harmonics.first().map(|row| row.order), Some(0));
        let fundamental = result.harmonics.iter().find(|row| row.order == 1).unwrap();
        assert!(fundamental.amplitude > 0.1);
    }
}
