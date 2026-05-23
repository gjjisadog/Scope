#[derive(Clone, Debug, Default)]
pub struct Dq0Result {
    pub d: Vec<f32>,
    pub q: Vec<f32>,
    pub zero: Vec<f32>,
}

fn finite_triplet(a: f32, b: f32, c: f32) -> bool {
    a.is_finite() && b.is_finite() && c.is_finite()
}

fn wrap_radians(angle: f64) -> f64 {
    angle.rem_euclid(std::f64::consts::TAU)
}

fn clarke(a: f64, b: f64, c: f64) -> (f64, f64) {
    let alpha = (2.0 / 3.0) * (a - 0.5 * b - 0.5 * c);
    let beta = (2.0 / 3.0) * ((3.0_f64.sqrt() * 0.5) * (b - c));
    (alpha, beta)
}

pub fn run_srf_pll(
    phase_a: &[f32],
    phase_b: &[f32],
    phase_c: &[f32],
    sample_rate_hz: f64,
    nominal_hz: f64,
) -> Result<Vec<f32>, String> {
    let sample_count = phase_a.len().min(phase_b.len()).min(phase_c.len());
    if sample_count < 16 {
        return Err("PLL needs at least 16 samples.".to_owned());
    }
    if sample_rate_hz <= 0.0 || nominal_hz <= 0.0 {
        return Err("PLL sample rate and nominal frequency must be positive.".to_owned());
    }

    let Some(first_index) = (0..sample_count)
        .find(|&index| finite_triplet(phase_a[index], phase_b[index], phase_c[index]))
    else {
        return Err("PLL source does not contain finite three-phase samples.".to_owned());
    };
    let (first_alpha, first_beta) = clarke(
        phase_a[first_index] as f64,
        phase_b[first_index] as f64,
        phase_c[first_index] as f64,
    );
    let mut theta = first_beta.atan2(first_alpha);
    if !theta.is_finite() {
        theta = 0.0;
    }

    let dt = 1.0 / sample_rate_hz;
    let nominal_omega = std::f64::consts::TAU * nominal_hz;
    let bandwidth = std::f64::consts::TAU * 20.0;
    let damping = 0.707;
    let kp = 2.0 * damping * bandwidth;
    let ki = bandwidth * bandwidth;
    let min_omega = nominal_omega * 0.2;
    let max_omega = nominal_omega * 2.0;
    let integrator_limit = nominal_omega;
    let mut integrator = 0.0_f64;
    let mut output = Vec::with_capacity(sample_count);

    for index in 0..sample_count {
        let a = phase_a[index];
        let b = phase_b[index];
        let c = phase_c[index];
        if !finite_triplet(a, b, c) {
            output.push(f32::NAN);
            continue;
        }

        let (alpha, beta) = clarke(a as f64, b as f64, c as f64);
        let sin = theta.sin();
        let cos = theta.cos();
        let q = -alpha * sin + beta * cos;
        let magnitude = (alpha * alpha + beta * beta).sqrt().max(1.0e-6);
        let error = (q / magnitude).clamp(-1.0, 1.0);

        integrator = (integrator + ki * error * dt).clamp(-integrator_limit, integrator_limit);
        let omega = (nominal_omega + kp * error + integrator).clamp(min_omega, max_omega);
        theta = wrap_radians(theta + omega * dt);
        output.push(theta as f32);
    }

    Ok(output)
}

pub fn abc_to_dq0(
    phase_a: &[f32],
    phase_b: &[f32],
    phase_c: &[f32],
    theta: &[f32],
) -> Result<Dq0Result, String> {
    let sample_count = phase_a
        .len()
        .min(phase_b.len())
        .min(phase_c.len())
        .min(theta.len());
    if sample_count < 16 {
        return Err("dq0 transform needs at least 16 samples.".to_owned());
    }

    let mut d = Vec::with_capacity(sample_count);
    let mut q = Vec::with_capacity(sample_count);
    let mut zero = Vec::with_capacity(sample_count);
    for index in 0..sample_count {
        let a = phase_a[index];
        let b = phase_b[index];
        let c = phase_c[index];
        let angle = theta[index];
        if !finite_triplet(a, b, c) || !angle.is_finite() {
            d.push(f32::NAN);
            q.push(f32::NAN);
            zero.push(f32::NAN);
            continue;
        }
        let (alpha, beta) = clarke(a as f64, b as f64, c as f64);
        let sin = (angle as f64).sin();
        let cos = (angle as f64).cos();
        d.push((alpha * cos + beta * sin) as f32);
        q.push((-alpha * sin + beta * cos) as f32);
        zero.push(((a as f64 + b as f64 + c as f64) / 3.0) as f32);
    }

    Ok(Dq0Result { d, q, zero })
}

pub fn radians_to_wrapped_degrees(theta: &[f32]) -> Vec<f32> {
    theta
        .iter()
        .map(|angle| {
            if angle.is_finite() {
                (wrap_radians(*angle as f64).to_degrees()) as f32
            } else {
                f32::NAN
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balanced_samples(
        sample_rate: f64,
        frequency: f64,
        amplitude: f64,
        phase: f64,
        count: usize,
    ) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let sample = |phase_offset: f64| {
            (0..count)
                .map(|index| {
                    let t = index as f64 / sample_rate;
                    (amplitude
                        * (std::f64::consts::TAU * frequency * t + phase + phase_offset).cos())
                        as f32
                })
                .collect::<Vec<_>>()
        };
        (
            sample(0.0),
            sample(-2.0 * std::f64::consts::PI / 3.0),
            sample(2.0 * std::f64::consts::PI / 3.0),
        )
    }

    fn unwrap(theta: &[f32]) -> Vec<f64> {
        let mut out = Vec::with_capacity(theta.len());
        let mut offset = 0.0;
        let mut previous = None;
        for &angle in theta {
            let current = angle as f64 + offset;
            if let Some(previous_angle) = previous {
                if current < previous_angle {
                    offset += std::f64::consts::TAU;
                }
            }
            let unwrapped = angle as f64 + offset;
            previous = Some(unwrapped);
            out.push(unwrapped);
        }
        out
    }

    #[test]
    fn srf_pll_locks_to_balanced_three_phase_voltage() {
        let sample_rate = 10_000.0;
        let frequency = 50.0;
        let (a, b, c) = balanced_samples(sample_rate, frequency, 1.0, 0.3, 20_000);
        let theta = run_srf_pll(&a, &b, &c, sample_rate, frequency).unwrap();
        let unwrapped = unwrap(&theta);
        let start = unwrapped[10_000];
        let end = unwrapped[19_999];
        let measured_hz =
            (end - start) / std::f64::consts::TAU / ((19_999 - 10_000) as f64 / sample_rate);

        assert!((measured_hz - frequency).abs() < 0.2);
        assert!(unwrapped.windows(2).all(|pair| pair[1] >= pair[0]));
    }

    #[test]
    fn dq0_of_balanced_signal_has_expected_d_axis() {
        let sample_rate = 10_000.0;
        let frequency = 50.0;
        let amplitude = 7.0;
        let (a, b, c) = balanced_samples(sample_rate, frequency, amplitude, 0.0, 20_000);
        let theta = run_srf_pll(&a, &b, &c, sample_rate, frequency).unwrap();
        let dq0 = abc_to_dq0(&a, &b, &c, &theta).unwrap();
        let tail = 10_000..20_000;
        let d_mean = tail.clone().map(|i| dq0.d[i] as f64).sum::<f64>() / tail.len() as f64;
        let q_rms = (tail.clone().map(|i| (dq0.q[i] as f64).powi(2)).sum::<f64>()
            / tail.len() as f64)
            .sqrt();
        let zero_rms = (tail
            .clone()
            .map(|i| (dq0.zero[i] as f64).powi(2))
            .sum::<f64>()
            / tail.len() as f64)
            .sqrt();

        assert!((d_mean - amplitude).abs() < 0.4);
        assert!(q_rms < 0.4);
        assert!(zero_rms < 0.001);
    }

    #[test]
    fn voltage_angle_decomposes_shifted_current() {
        let sample_rate = 10_000.0;
        let frequency = 50.0;
        let (va, vb, vc) = balanced_samples(sample_rate, frequency, 1.0, 0.0, 20_000);
        let current_amp = 5.0;
        let shift = 30.0_f64.to_radians();
        let (ia, ib, ic) = balanced_samples(sample_rate, frequency, current_amp, shift, 20_000);
        let theta = run_srf_pll(&va, &vb, &vc, sample_rate, frequency).unwrap();
        let dq0 = abc_to_dq0(&ia, &ib, &ic, &theta).unwrap();
        let tail = 10_000..20_000;
        let d_mean = tail.clone().map(|i| dq0.d[i] as f64).sum::<f64>() / tail.len() as f64;
        let q_mean = tail.clone().map(|i| dq0.q[i] as f64).sum::<f64>() / tail.len() as f64;

        assert!((d_mean - current_amp * shift.cos()).abs() < 0.4);
        assert!((q_mean - current_amp * shift.sin()).abs() < 0.4);
    }

    #[test]
    fn rejects_short_or_non_finite_sources() {
        let short = vec![0.0_f32; 8];
        assert!(run_srf_pll(&short, &short, &short, 1000.0, 50.0).is_err());

        let bad = vec![f32::NAN; 32];
        assert!(run_srf_pll(&bad, &bad, &bad, 1000.0, 50.0).is_err());
    }
}
