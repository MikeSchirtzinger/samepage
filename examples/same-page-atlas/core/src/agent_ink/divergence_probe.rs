//! Cross-target floating-point divergence probe for agent ink B0.
//!
//! This is a standalone measurement program, not production solver code. Compile
//! this file directly for the native host and as a `wasm32-unknown-unknown`
//! `cdylib`, then compare the exported `f64::to_bits()` traces.

const STEPS: u32 = 400;
const VALUES: usize = 8;
const INITIAL: [f64; VALUES] = [0.31, -0.23, 1.74, 0.41, 2.62, 1.63, 0.72, 2.34];

fn distance(values: &[f64; VALUES], a: usize, b: usize) -> f64 {
    let dx = values[2 * a] - values[2 * b];
    let dy = values[2 * a + 1] - values[2 * b + 1];
    (dx * dx + dy * dy).sqrt()
}

fn signed_angle(values: &[f64; VALUES], a: usize, pivot: usize, b: usize) -> f64 {
    let ax = values[2 * a] - values[2 * pivot];
    let ay = values[2 * a + 1] - values[2 * pivot + 1];
    let bx = values[2 * b] - values[2 * pivot];
    let by = values[2 * b + 1] - values[2 * pivot + 1];
    let cross = ax * by - ay * bx;
    let dot = ax * bx + ay * by;
    cross.atan2(dot)
}

fn square(value: f64) -> f64 {
    value * value
}

/// One fixed nonlinear least-squares system over four 2D points.
///
/// The pin residuals remove rigid translation and rotation. Four edge lengths,
/// one diagonal, and two signed angles supply the nonlinear relations.
fn objective(values: &[f64; VALUES]) -> f64 {
    let residuals = [
        4.0 * values[0],
        4.0 * values[1],
        3.0 * values[3],
        distance(values, 0, 1) - 1.85,
        distance(values, 1, 2) - 1.42,
        distance(values, 2, 3) - 1.68,
        distance(values, 3, 0) - 2.08,
        distance(values, 0, 2) - 2.47,
        signed_angle(values, 0, 1, 2) + 1.09,
        signed_angle(values, 1, 2, 3) + 1.27,
    ];
    residuals.into_iter().map(square).sum()
}

fn gradient(values: &[f64; VALUES]) -> [f64; VALUES] {
    let mut result = [0.0; VALUES];
    for index in 0..VALUES {
        let h = 1.0e-6 * (1.0 + values[index].abs());
        let mut lower = *values;
        let mut upper = *values;
        lower[index] -= h;
        upper[index] += h;
        result[index] = (objective(&upper) - objective(&lower)) / (2.0 * h);
    }
    result
}

/// Advance one warm-started gradient-descent step with Armijo backtracking.
fn advance(values: [f64; VALUES]) -> [f64; VALUES] {
    let current = objective(&values);
    let gradient = gradient(&values);
    let gradient_norm_squared: f64 = gradient.iter().map(|value| value * value).sum();
    if gradient_norm_squared <= 1.0e-30 {
        return values;
    }

    let mut scale = 0.08;
    for _ in 0..24 {
        let mut candidate = values;
        for index in 0..VALUES {
            candidate[index] -= scale * gradient[index];
        }
        let sufficient_decrease = current - 1.0e-4 * scale * gradient_norm_squared;
        if objective(&candidate) <= sufficient_decrease {
            return candidate;
        }
        scale *= 0.5;
    }
    values
}

fn state_at(step: u32) -> [f64; VALUES] {
    let mut values = INITIAL;
    for _ in 0..step.min(STEPS) {
        values = advance(values);
    }
    values
}

fn mix(mut digest: u64, value: u64) -> u64 {
    for byte in value.to_le_bytes() {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
    }
    digest
}

fn step_digest(values: &[f64; VALUES]) -> u64 {
    values.iter().fold(0xcbf2_9ce4_8422_2325, |digest, value| {
        mix(digest, value.to_bits())
    })
}

fn trace_digest() -> u64 {
    let mut digest = 0xcbf2_9ce4_8422_2325;
    let mut values = INITIAL;
    for step in 0..=STEPS {
        digest = mix(digest, u64::from(step));
        for value in values {
            digest = mix(digest, value.to_bits());
        }
        if step != STEPS {
            values = advance(values);
        }
    }
    digest
}

#[no_mangle]
pub extern "C" fn agent_ink_probe_steps() -> u32 {
    STEPS
}

#[no_mangle]
pub extern "C" fn agent_ink_probe_values() -> u32 {
    VALUES as u32
}

#[no_mangle]
pub extern "C" fn agent_ink_probe_bits(step: u32, index: u32) -> u64 {
    state_at(step)
        .get(index as usize)
        .copied()
        .unwrap_or(f64::NAN)
        .to_bits()
}

#[no_mangle]
pub extern "C" fn agent_ink_probe_step_digest(step: u32) -> u64 {
    step_digest(&state_at(step))
}

#[no_mangle]
pub extern "C" fn agent_ink_probe_trace_digest() -> u64 {
    trace_digest()
}

#[no_mangle]
pub extern "C" fn agent_ink_probe_objective_bits(step: u32) -> u64 {
    objective(&state_at(step)).to_bits()
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    dead_code,
    reason = "used when this source file is compiled as a standalone probe"
)]
fn print_summary() {
    println!("target={}", std::env::consts::ARCH);
    println!("steps={STEPS}");
    println!("values_per_step={VALUES}");
    println!("trace_digest=0x{:016x}", trace_digest());
    for step in [0, 1, 100, 200, 300, 400] {
        println!(
            "milestone_step={step} digest=0x{:016x}",
            step_digest(&state_at(step))
        );
    }
    let final_values = state_at(STEPS);
    let bits = final_values
        .iter()
        .map(|value| format!("{:016x}", value.to_bits()))
        .collect::<Vec<_>>()
        .join(",");
    println!("final_bits={bits}");
    println!(
        "final_objective_bits=0x{:016x}",
        objective(&final_values).to_bits()
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    dead_code,
    reason = "used when this source file is compiled as a standalone probe"
)]
fn print_trace() {
    let mut values = INITIAL;
    for step in 0..=STEPS {
        let bits = values
            .iter()
            .map(|value| format!("{:016x}", value.to_bits()))
            .collect::<Vec<_>>()
            .join(",");
        println!("{step}:{bits}");
        if step != STEPS {
            values = advance(values);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    dead_code,
    reason = "used when this source file is compiled as a standalone probe"
)]
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--trace") {
        print_trace();
    } else {
        print_summary();
    }
}
