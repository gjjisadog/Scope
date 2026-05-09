use rustfft::{num_complex::Complex, FftPlanner};

#[derive(Clone, Debug, Default)]
pub struct HarmonicRow {
    pub order: usize,
    pub frequency_hz: f64,
    pub amplitude: f32,
    pub phase_deg: f32,
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

#[derive(Clone, Debug, Default)]
pub struct SequenceComponent {
    pub name: &'static str,
    pub amplitude: f32,
    pub phase_deg: f32,
    pub percent_of_positive: f32,
}

#[derive(Clone, Debug, Default)]
pub struct SequenceResult {
    pub group_name: String,
    pub fundamental_hz: f64,
    pub phase_a_deg: f32,
    pub phase_b_deg: f32,
    pub phase_c_deg: f32,
    pub zero: SequenceComponent,
    pub positive: SequenceComponent,
    pub negative: SequenceComponent,
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

fn scaled_phasor(buffer: &[Complex<f32>], bin: usize, sample_count: usize) -> Complex<f32> {
    let scale = 2.0 / sample_count as f32;
    buffer[bin] * scale
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

    let buffer = fft_buffer(samples);
    let len = buffer.len();
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
        let phase_deg = phase_deg(buffer[bin]);
        harmonics.push(HarmonicRow {
            order,
            frequency_hz: bin as f64 * sample_rate_hz / len as f64,
            amplitude,
            phase_deg,
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

pub fn analyze_sequence(
    group_name: String,
    samples_a: &[f32],
    samples_b: &[f32],
    samples_c: &[f32],
    sample_rate_hz: f64,
) -> Option<SequenceResult> {
    let sample_count = samples_a.len().min(samples_b.len()).min(samples_c.len());
    if sample_count < 16 || sample_rate_hz <= 0.0 {
        return None;
    }

    let buffer_a = fft_buffer(&samples_a[..sample_count]);
    let buffer_b = fft_buffer(&samples_b[..sample_count]);
    let buffer_c = fft_buffer(&samples_c[..sample_count]);
    let len = buffer_a.len();
    let half = len / 2;
    let scale = 2.0 / sample_count as f32;

    let (fundamental_index, _) = (1..half)
        .map(|bin| (bin, buffer_a[bin].norm() * scale))
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;
    let fundamental_hz = fundamental_index as f64 * sample_rate_hz / len as f64;

    let va = scaled_phasor(&buffer_a, fundamental_index, sample_count);
    let vb = scaled_phasor(&buffer_b, fundamental_index, sample_count);
    let vc = scaled_phasor(&buffer_c, fundamental_index, sample_count);
    let alpha = Complex::from_polar(1.0_f32, 120.0_f32.to_radians());
    let alpha2 = Complex::from_polar(1.0_f32, 240.0_f32.to_radians());
    let third = 1.0_f32 / 3.0_f32;

    let zero = (va + vb + vc) * third;
    let positive = (va + alpha * vb + alpha2 * vc) * third;
    let negative = (va + alpha2 * vb + alpha * vc) * third;
    let positive_amp = positive.norm();

    let component = |name: &'static str, value: Complex<f32>| SequenceComponent {
        name,
        amplitude: value.norm(),
        phase_deg: phase_deg(value),
        percent_of_positive: if positive_amp > 0.0 {
            value.norm() / positive_amp * 100.0
        } else {
            0.0
        },
    };

    Some(SequenceResult {
        group_name,
        fundamental_hz,
        phase_a_deg: phase_deg(va),
        phase_b_deg: phase_deg(vb),
        phase_c_deg: phase_deg(vc),
        zero: component("Zero", zero),
        positive: component("Positive", positive),
        negative: component("Negative", negative),
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
