//! A tiny feedforward policy network, written from scratch (no dependencies).
//!
//! One hidden layer with tanh, sigmoid output. It maps a transfer situation
//! to P(bid): inputs are the same signals a manager weighs — need, value,
//! wage room, ambition, reputation gap, request status — so identical deals
//! resolve differently per individual through their trait inputs. Weights are
//! learned by the evolution-strategies trainer in tune.rs, not hand-set.

/// Sigmoid activation.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn tanh(x: f32) -> f32 {
    x.tanh()
}

/// Number of inputs: need, value_fit, wage_room, ambition, rep_gap, requested.
pub const NET_INPUTS: usize = 6;
/// Hidden units: small on purpose — the decision boundary should stay simple.
pub const NET_HIDDEN: usize = 8;

pub const NET_WEIGHTS: usize = NET_INPUTS * NET_HIDDEN + NET_HIDDEN + NET_HIDDEN + 1;

/// Forward pass. `w` is exactly NET_WEIGHTS floats: input->hidden weights,
/// hidden biases, hidden->output weights, output bias.
pub fn forward(w: &[f32], inputs: &[f32; NET_INPUTS]) -> f32 {
    debug_assert_eq!(w.len(), NET_WEIGHTS);
    let mut h = [0.0f32; NET_HIDDEN];
    for j in 0..NET_HIDDEN {
        let mut s = w[NET_INPUTS * NET_HIDDEN + j];
        for i in 0..NET_INPUTS {
            s += w[i * NET_HIDDEN + j] * inputs[i];
        }
        h[j] = tanh(s);
    }
    let base = NET_INPUTS * NET_HIDDEN + NET_HIDDEN;
    let mut out = w[base + NET_HIDDEN];
    for j in 0..NET_HIDDEN {
        out += w[base + j] * h[j];
    }
    sigmoid(out)
}

/// Default weights: mimic the old hand rule (bid on clear need + fair value),
/// so behavior starts sane before training moves it.
pub fn default_weights() -> Vec<f32> {
    // Hand-set prior: need and value dominate, ambition/wage modulate.
    // input order: need, value_fit, wage_room, ambition, rep_gap, requested.
    let priors = [
        [0.9, 0.2, -0.3, 0.4, 0.1, -0.2, 0.5, 0.0], // need
        [0.5, 0.6, 0.1, -0.2, 0.3, 0.2, -0.1, 0.4], // value_fit
        [0.2, 0.1, 0.7, 0.2, -0.1, 0.3, 0.2, -0.2], // wage_room
        [0.3, -0.1, 0.2, 0.5, 0.4, 0.1, 0.0, 0.3], // ambition
        [-0.4, 0.2, 0.1, -0.3, 0.5, -0.2, 0.3, 0.1], // rep_gap
        [0.4, 0.3, -0.1, 0.2, 0.1, 0.5, -0.3, 0.2], // requested
    ];
    let mut w = Vec::with_capacity(NET_WEIGHTS);
    for i in 0..NET_INPUTS {
        for j in 0..NET_HIDDEN {
            w.push(priors[i][j]);
        }
    }
    for _ in 0..NET_HIDDEN {
        w.push(0.0); // hidden biases
    }
    for j in 0..NET_HIDDEN {
        w.push(if j % 2 == 0 { 0.6 } else { 0.4 }); // output weights
    }
    w.push(-0.9); // output bias: default bar ≈ bid on genuine need
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_probability() {
        let w = default_weights();
        let p = forward(&w, &[0.5; NET_INPUTS]);
        assert!((0.0..=1.0).contains(&p));
    }

    #[test]
    fn clear_need_bids_and_hopeless_passes() {
        let w = default_weights();
        // screaming need + bargain + room + ambitious + lateral + requested
        assert!(forward(&w, &[1.0, 1.0, 1.0, 1.0, 0.5, 1.0]) > 0.5);
        // no need, overpriced, no room, cautious, big step down, settled
        assert!(forward(&w, &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0]) < 0.5);
    }

    #[test]
    fn deterministic_per_weights() {
        let w = default_weights();
        let a = forward(&w, &[0.3, 0.7, 0.2, 0.9, 0.4, 0.0]);
        let b = forward(&w, &[0.3, 0.7, 0.2, 0.9, 0.4, 0.0]);
        assert_eq!(a, b);
    }

    #[test]
    fn traits_change_identical_deals() {
        // Same deal, different ambition inputs → different outputs possible.
        let w = default_weights();
        let low = forward(&w, &[0.4, 0.4, 0.4, 0.0, 0.5, 0.0]);
        let high = forward(&w, &[0.4, 0.4, 0.4, 1.0, 0.5, 0.0]);
        assert!((high - low).abs() > 1e-6);
    }
}
