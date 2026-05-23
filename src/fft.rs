use rustfft::num_complex::Complex;

#[derive(Clone, Debug, Default)]
pub struct HarmonicRow {
    pub order: usize,
    pub amplitude: f32,
    pub phase_deg: f32,
    pub relative_percent: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FftResult {
    pub sample_count: usize,
    pub thd_percent: f32,
    pub harmonics: Vec<HarmonicRow>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SequenceComponent {
    pub amplitude: f32,
    pub phase_deg: f32,
    pub relative_percent: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SequenceResult {
    pub zero: SequenceComponent,
    pub positive: SequenceComponent,
    pub negative: SequenceComponent,
    pub sample_count: usize,
}

fn hann_window(index: usize, len: usize) -> f64 {
    let denom = len.saturating_sub(1).max(1) as f64;
    0.5 - 0.5 * (std::f64::consts::TAU * index as f64 / denom).cos()
}

fn finite_mean(samples: &[f32]) -> Option<(f64, usize)> {
    let mut sum = 0.0_f64;
    let mut count = 0_usize;
    for sample in samples.iter().copied().filter(|sample| sample.is_finite()) {
        sum += sample as f64;
        count += 1;
    }
    (count >= 16).then_some((sum / count as f64, count))
}

fn harmonic_phasor(
    samples: &[f32],
    sample_rate_hz: f64,
    frequency_hz: f64,
) -> Option<Complex<f32>> {
    if samples.len() < 16
        || sample_rate_hz <= 0.0
        || frequency_hz <= 0.0
        || frequency_hz >= sample_rate_hz * 0.5
    {
        return None;
    }

    let (mean, finite_count) = finite_mean(samples)?;
    let mut sum_re = 0.0_f64;
    let mut sum_im = 0.0_f64;
    let mut window_sum = 0.0_f64;

    for (index, sample) in samples.iter().copied().enumerate() {
        if !sample.is_finite() {
            continue;
        }
        let window = hann_window(index, samples.len());
        let centered = (sample as f64 - mean) * window;
        let angle = std::f64::consts::TAU * frequency_hz * index as f64 / sample_rate_hz;
        sum_re += centered * angle.cos();
        sum_im -= centered * angle.sin();
        window_sum += window;
    }

    if finite_count < 16 || window_sum <= f64::EPSILON {
        return None;
    }

    let scale = 2.0 / window_sum;
    Some(Complex::new(
        (sum_re * scale) as f32,
        (sum_im * scale) as f32,
    ))
}

fn phase_deg(value: Complex<f32>) -> f32 {
    value.arg().to_degrees()
}

pub fn fundamental_phasor(
    samples: &[f32],
    sample_rate_hz: f64,
    harmonic_base_hz: f64,
) -> Option<Complex<f32>> {
    harmonic_phasor(samples, sample_rate_hz, harmonic_base_hz)
}

pub fn sequence_components(
    phase_a: &[f32],
    phase_b: &[f32],
    phase_c: &[f32],
    sample_rate_hz: f64,
    harmonic_base_hz: f64,
) -> Option<SequenceResult> {
    let sample_count = phase_a.len().min(phase_b.len()).min(phase_c.len());
    if sample_count < 16 {
        return None;
    }
    let va = fundamental_phasor(&phase_a[..sample_count], sample_rate_hz, harmonic_base_hz)?;
    let vb = fundamental_phasor(&phase_b[..sample_count], sample_rate_hz, harmonic_base_hz)?;
    let vc = fundamental_phasor(&phase_c[..sample_count], sample_rate_hz, harmonic_base_hz)?;

    let a = Complex::from_polar(1.0, 2.0 * std::f32::consts::PI / 3.0);
    let a2 = a * a;
    let one_third = Complex::new(1.0 / 3.0, 0.0);
    let zero = (va + vb + vc) * one_third;
    let positive = (va + a * vb + a2 * vc) * one_third;
    let negative = (va + a2 * vb + a * vc) * one_third;

    let positive_amp = positive.norm();
    let component = |value: Complex<f32>| -> SequenceComponent {
        let amplitude = value.norm();
        let relative_percent = if positive_amp > 0.0 {
            amplitude / positive_amp * 100.0
        } else {
            0.0
        };
        SequenceComponent {
            amplitude,
            phase_deg: phase_deg(value),
            relative_percent,
        }
    };

    Some(SequenceResult {
        zero: component(zero),
        positive: component(positive),
        negative: component(negative),
        sample_count,
    })
}

pub fn analyze(
    _channel_name: String,
    samples: &[f32],
    sample_rate_hz: f64,
    harmonic_base_hz: f64,
    harmonic_count: usize,
) -> Option<FftResult> {
    if samples.len() < 16 || sample_rate_hz <= 0.0 || harmonic_base_hz <= 0.0 {
        return None;
    }

    let (mean, finite_count) = finite_mean(samples)?;
    let dc_amplitude = mean.abs() as f32;
    let fundamental = harmonic_phasor(samples, sample_rate_hz, harmonic_base_hz)?;
    let fundamental_amp = fundamental.norm();

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
        let Some(phasor) =
            harmonic_phasor(samples, sample_rate_hz, harmonic_base_hz * order as f64)
        else {
            break;
        };
        let amplitude = phasor.norm();
        if order > 1 {
            harmonic_power += (amplitude as f64).powi(2);
        }
        let relative_percent = if fundamental_amp > 0.0 {
            amplitude / fundamental_amp * 100.0
        } else {
            0.0
        };
        let phase_deg = phase_deg(phasor);
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
        sample_count: finite_count,
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
        assert!((fundamental.amplitude - 1.0).abs() < 0.01);
        assert!((fundamental.phase_deg + 90.0).abs() < 0.5);
    }

    #[test]
    fn reports_calibrated_cosine_amplitude_phase_and_dc() {
        let sample_rate = 10_000.0;
        let frequency = 1_000.0;
        let amplitude = 3.5;
        let phase_deg = 30.0_f64;
        let dc = 1.25_f64;
        let phase = phase_deg.to_radians();
        let samples = (0..5000)
            .map(|i| {
                (dc + amplitude
                    * (std::f64::consts::TAU * frequency * i as f64 / sample_rate + phase).cos())
                    as f32
            })
            .collect::<Vec<_>>();

        let result = analyze("CH1".to_owned(), &samples, sample_rate, frequency, 5).unwrap();
        let dc_row = result.harmonics.iter().find(|row| row.order == 0).unwrap();
        let fundamental = result.harmonics.iter().find(|row| row.order == 1).unwrap();

        assert_eq!(result.sample_count, samples.len());
        assert!((dc_row.amplitude - dc as f32).abs() < 0.001);
        assert!((fundamental.amplitude - amplitude as f32).abs() < 0.01);
        assert!((fundamental.phase_deg - phase_deg as f32).abs() < 0.5);
    }

    #[test]
    fn reports_known_second_harmonic_thd() {
        let sample_rate = 10_000.0;
        let frequency = 50.0;
        let fundamental_amp = 10.0;
        let second_amp = 1.0;
        let samples = (0..10_000)
            .map(|i| {
                let t = i as f64 / sample_rate;
                (fundamental_amp * (std::f64::consts::TAU * frequency * t).cos()
                    + second_amp * (std::f64::consts::TAU * 2.0 * frequency * t).cos())
                    as f32
            })
            .collect::<Vec<_>>();

        let result = analyze("CH1".to_owned(), &samples, sample_rate, frequency, 5).unwrap();
        let second = result.harmonics.iter().find(|row| row.order == 2).unwrap();

        assert!((second.amplitude - second_amp as f32).abs() < 0.01);
        assert!((second.relative_percent - 10.0).abs() < 0.2);
        assert!((result.thd_percent - 10.0).abs() < 0.2);
    }

    #[test]
    fn handles_non_bin_aligned_fundamental_frequency() {
        let sample_rate = 10_000.0;
        let frequency = 997.3;
        let amplitude = 2.0;
        let phase_deg = -47.0_f64;
        let phase = phase_deg.to_radians();
        let samples = (0..4096)
            .map(|i| {
                (amplitude
                    * (std::f64::consts::TAU * frequency * i as f64 / sample_rate + phase).cos())
                    as f32
            })
            .collect::<Vec<_>>();

        let result = analyze("CH1".to_owned(), &samples, sample_rate, frequency, 3).unwrap();
        let fundamental = result.harmonics.iter().find(|row| row.order == 1).unwrap();

        assert!((fundamental.amplitude - amplitude as f32).abs() < 0.03);
        assert!((fundamental.phase_deg - phase_deg as f32).abs() < 1.0);
    }

    #[test]
    fn reports_balanced_positive_sequence_components() {
        let sample_rate = 10_000.0;
        let frequency = 50.0;
        let amplitude = 5.0;
        let phase_a = 20.0_f64.to_radians();
        let phase_b = (20.0_f64 - 120.0).to_radians();
        let phase_c = (20.0_f64 + 120.0).to_radians();
        let samples = |phase: f64| {
            (0..10_000)
                .map(|i| {
                    (amplitude
                        * (std::f64::consts::TAU * frequency * i as f64 / sample_rate + phase)
                            .cos()) as f32
                })
                .collect::<Vec<_>>()
        };

        let result = sequence_components(
            &samples(phase_a),
            &samples(phase_b),
            &samples(phase_c),
            sample_rate,
            frequency,
        )
        .unwrap();

        assert!((result.positive.amplitude - amplitude as f32).abs() < 0.01);
        assert!(result.zero.amplitude < 0.01);
        assert!(result.negative.amplitude < 0.01);
        assert!((result.positive.phase_deg - 20.0).abs() < 0.5);
    }
}
