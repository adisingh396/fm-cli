//! Small feedforward decision models, written from scratch (no dependencies).
//!
//! The simulation uses small, auditable policy networks rather than one giant
//! black box. Each decision type has its own tiny model:
//! - player acceptance/contract preference
//! - manager / club financial risk and willingness
//! - scouting priority and prospect valuation
//! - transfer bidding (legacy six-input policy, kept for compatibility)
//!
//! Every forward pass is deterministic for a given weight vector, with
//! features normalized by the caller. Actual money movement, wages, budget
//! checks, and transfers remain deterministic rules around the learned score.
//
//! This design is intentionally offline and reproducible. It is easy to export
//! the vectors to a GPU training script later, but the live simulation does
//! not require a GPU: the whole network library is only a few hundred floats.

use serde::{Deserialize, Serialize};

/// Sigmoid activation.
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn tanh(x: f32) -> f32 {
    x.tanh()
}

/// A complete audit record for one neural decision. This is the user-facing
/// debugging surface: instead of guessing why a player refused, rejected a
/// bid or left a club, the record contains the model inputs and each feature's
/// first-layer contribution to the output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionAudit {
    pub model: String,
    pub kind: String,
    pub context: String,
    pub output: f32,
    pub inputs: Vec<(String, f32)>,
    pub contributions: Vec<(String, f32)>,
}

/// Append a JSON audit record, if the caller requests it via FM_AUDIT_LOG.
pub fn append_audit(record: &DecisionAudit) {
    if std::env::var("FM_AUDIT_LOG").unwrap_or_default() != "1" {
        return;
    }
    let _ = std::fs::create_dir_all("sim_dump");
    let line = serde_json::to_string(record).unwrap_or_default();
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sim_dump/ml_decisions.jsonl")
    {
        let _ = writeln!(file, "{}", line);
    }
}

/// One small hidden-layer network with independent feature names for audit.
#[derive(Clone, Debug)]
pub struct SmallMlp {
    pub model: String,
    pub feature_names: Vec<&'static str>,
    pub weights: Vec<f32>,
    pub hidden: usize,
}

impl SmallMlp {
    pub fn new(
        model: &str,
        feature_names: Vec<&'static str>,
        weights: Vec<f32>,
        hidden: usize,
    ) -> Self {
        SmallMlp {
            model: model.into(),
            feature_names,
            weights,
            hidden,
        }
    }

    pub fn forward(&self, inputs: &[f32]) -> f32 {
        let input_count = self.feature_names.len();
        let expected = input_count * self.hidden + self.hidden + self.hidden + 1;
        if self.weights.len() != expected || inputs.len() != input_count {
            return 0.5;
        }
        let mut h = vec![0.0f32; self.hidden];
        for j in 0..self.hidden {
            let mut s = self.weights[input_count * self.hidden + j];
            for i in 0..input_count {
                s += self.weights[i * self.hidden + j] * inputs[i];
            }
            h[j] = tanh(s);
        }
        let base = input_count * self.hidden + self.hidden;
        let mut out = self.weights[base + self.hidden];
        for j in 0..self.hidden {
            out += self.weights[base + j] * h[j];
        }
        sigmoid(out)
    }

    pub fn forward_batch(&self, features: &[Vec<f32>]) -> Vec<f32> {
        features.iter().map(|x| self.forward(x)).collect()
    }

    pub fn audit(&self, kind: &str, context: &str, inputs: &[f32]) -> (f32, DecisionAudit) {
        let output = self.forward(inputs);
        let input_count = self.feature_names.len();
        let mut contributions = Vec::with_capacity(input_count);
        if self.weights.len() == input_count * self.hidden + self.hidden + self.hidden + 1
            && inputs.len() == input_count
        {
            for i in 0..input_count {
                let mut c = 0.0;
                for j in 0..self.hidden {
                    c += self.weights[i * self.hidden + j] * inputs[i];
                }
                contributions.push((self.feature_names[i].into(), c));
            }
        }
        let record = DecisionAudit {
            model: self.model.clone(),
            kind: kind.into(),
            context: context.into(),
            output,
            inputs: self
                .feature_names
                .iter()
                .cloned()
                .zip(inputs.iter().copied())
                .map(|(n, v)| (n.into(), v))
                .collect(),
            contributions,
        };
        (output, record)
    }
}

pub const PLAYER_INPUTS: &[&str] = &[
    "age_norm",
    "overall",
    "pace",
    "stamina",
    "potential_gap",
    "offer_ratio",
    "club_stage",
    "morale",
    "ambition",
    "greed",
    "loyalty",
    "contract_happiness",
];
pub const MANAGER_INPUTS: &[&str] = &[
    "wage_room",
    "squad_age",
    "title_drought",
    "prospects",
    "wealth",
    "willingness",
    "security",
    "ambition",
    "youth_trust",
    "reputation",
    "quality",
    "performance",
];
pub const SCOUT_INPUTS: &[&str] = &[
    "rating_gap",
    "potential",
    "age",
    "value_ratio",
    "squad_need",
    "one_rung_step",
    "seller_finance",
    "wage_fit",
    "academy",
    "risk",
    "pathway",
    "rep_gap",
];
pub const FINANCE_INPUTS: &[&str] = &[
    "wage_room",
    "revenue",
    "budget",
    "wage_ratio",
    "squad_strength",
    "prospects",
    "willingness",
    "club_value",
    "inflation",
    "academy",
    "ambition",
    "reputation",
];

fn small_weights(input_count: usize, hidden: usize, seed_fill: f32) -> Vec<f32> {
    let total = input_count * hidden + hidden + hidden + 1;
    let mut w = Vec::with_capacity(total);
    let mut x = seed_fill;
    for _ in 0..total {
        x = (x * 1103515245.0 + 12345.0) % 2147483648.0;
        w.push(((x / 2147483648.0) - 0.5) * 0.25);
    }
    let base = input_count * hidden + hidden;
    w[base + hidden] = -0.05;
    w
}

pub fn default_player_weights() -> Vec<f32> {
    small_weights(PLAYER_INPUTS.len(), PLAYER_HIDDEN, 11.0)
}
pub fn default_manager_weights() -> Vec<f32> {
    small_weights(MANAGER_INPUTS.len(), MANAGER_HIDDEN, 23.0)
}
pub fn default_scout_weights() -> Vec<f32> {
    small_weights(SCOUT_INPUTS.len(), SCOUT_HIDDEN, 37.0)
}
pub fn default_finance_weights() -> Vec<f32> {
    small_weights(FINANCE_INPUTS.len(), FINANCE_HIDDEN, 53.0)
}

pub fn player_net(weights: &[f32]) -> SmallMlp {
    SmallMlp::new("player", PLAYER_INPUTS.to_vec(), weights.to_vec(), 12)
}
pub fn manager_net(weights: &[f32]) -> SmallMlp {
    SmallMlp::new("manager", MANAGER_INPUTS.to_vec(), weights.to_vec(), 12)
}
pub fn scout_net(weights: &[f32]) -> SmallMlp {
    SmallMlp::new("scout", SCOUT_INPUTS.to_vec(), weights.to_vec(), 12)
}
pub fn finance_net(weights: &[f32]) -> SmallMlp {
    SmallMlp::new("finance", FINANCE_INPUTS.to_vec(), weights.to_vec(), 12)
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelSpec {
    pub model: String,
    pub features: Vec<String>,
    pub hidden: usize,
    pub weights: Vec<f32>,
}

/// Export the current policy vectors and feature schemas. A GPU training
/// script can consume these plain matrices without Rust/GPU bindings.
pub fn export_ml_models(w: &crate::tune::Params) -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            model: "player".into(),
            features: PLAYER_INPUTS.iter().map(|s| s.to_string()).collect(),
            hidden: PLAYER_HIDDEN,
            weights: w.player_policy.clone(),
        },
        ModelSpec {
            model: "manager".into(),
            features: MANAGER_INPUTS.iter().map(|s| s.to_string()).collect(),
            hidden: MANAGER_HIDDEN,
            weights: w.manager_policy.clone(),
        },
        ModelSpec {
            model: "scout".into(),
            features: SCOUT_INPUTS.iter().map(|s| s.to_string()).collect(),
            hidden: SCOUT_HIDDEN,
            weights: w.scout_policy.clone(),
        },
        ModelSpec {
            model: "finance".into(),
            features: FINANCE_INPUTS.iter().map(|s| s.to_string()).collect(),
            hidden: FINANCE_HIDDEN,
            weights: w.finance_policy.clone(),
        },
        ModelSpec {
            model: "transfer".into(),
            features: vec![
                "need".into(),
                "value_fit".into(),
                "wage_room".into(),
                "ambition".into(),
                "rep_gap".into(),
                "requested".into(),
            ],
            hidden: NET_HIDDEN,
            weights: w.policy.clone(),
        },
    ]
}

/// One model for a full feature schema, with first-layer output and a
/// reconstructable weight vector in the same order as Rust uses internally.
#[derive(Clone, Debug, Serialize)]
pub struct SerializedMlp {
    pub model: String,
    pub features: Vec<String>,
    pub hidden: usize,
    pub input_weights: Vec<f32>,
    pub hidden_bias: Vec<f32>,
    pub output_weights: Vec<f32>,
    pub output_bias: f32,
}

/// Convert a one-hidden-layer network into separate matrices. This is the
/// most GPU-friendly representation: the input and output layers are plain
/// contiguous `f32` arrays, while feature names remain source-readable.
pub fn serialize_mlp(model: &SmallMlp) -> Option<SerializedMlp> {
    let input_count = model.feature_names.len();
    let expected = input_count * model.hidden + model.hidden + model.hidden + 1;
    if model.weights.len() != expected {
        return None;
    }
    let base = input_count * model.hidden + model.hidden;
    Some(SerializedMlp {
        model: model.model.clone(),
        features: model.feature_names.iter().map(|s| s.to_string()).collect(),
        hidden: model.hidden,
        input_weights: model.weights[..input_count * model.hidden].to_vec(),
        hidden_bias: model.weights[input_count * model.hidden..base].to_vec(),
        output_weights: model.weights[base..base + model.hidden].to_vec(),
        output_bias: model.weights[base + model.hidden],
    })
}

pub fn write_ml_export(params: &crate::tune::Params, path: &str) -> std::io::Result<()> {
    let specs = export_ml_models(params);
    let serial = specs
        .iter()
        .filter_map(|spec| {
            let features: Vec<&'static str> = if spec.model == "player" {
                PLAYER_INPUTS.to_vec()
            } else if spec.model == "manager" {
                MANAGER_INPUTS.to_vec()
            } else if spec.model == "scout" {
                SCOUT_INPUTS.to_vec()
            } else if spec.model == "finance" {
                FINANCE_INPUTS.to_vec()
            } else if spec.model == "transfer" {
                vec![
                    "need",
                    "value_fit",
                    "wage_room",
                    "ambition",
                    "rep_gap",
                    "requested",
                ]
            } else {
                Vec::new()
            };
            serialize_mlp(&SmallMlp::new(
                &spec.model,
                features,
                spec.weights.clone(),
                spec.hidden,
            ))
        })
        .collect::<Vec<_>>();
    std::fs::create_dir_all(
        std::path::Path::new(path)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    )?;
    std::fs::write(path, serde_json::to_string_pretty(&serial)?)
}

/// Lightweight aggregate audit metrics for model outputs: min/max/mean/sd.
/// These are cheap sanity checks for validation, not a full training report.
pub fn output_stats(outputs: &[f32]) -> (f32, f32, f32, f32) {
    if outputs.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let min = outputs.iter().copied().fold(f32::INFINITY, f32::min);
    let max = outputs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mean = outputs.iter().sum::<f32>() / outputs.len() as f32;
    let var = outputs.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / outputs.len() as f32;
    (min, max, mean, var.sqrt())
}

/// Number of inputs: need, value_fit, wage_room, ambition, rep_gap, requested.
pub const NET_INPUTS: usize = 6;
/// Hidden units: small on purpose — the decision boundary should stay simple.
pub const NET_HIDDEN: usize = 8;

pub const PLAYER_HIDDEN: usize = 12;
pub const MANAGER_HIDDEN: usize = 12;
pub const SCOUT_HIDDEN: usize = 12;
pub const FINANCE_HIDDEN: usize = 12;

pub const PLAYER_WEIGHTS: usize =
    PLAYER_INPUTS.len() * PLAYER_HIDDEN + PLAYER_HIDDEN + PLAYER_HIDDEN + 1;
pub const MANAGER_WEIGHTS: usize =
    MANAGER_INPUTS.len() * MANAGER_HIDDEN + MANAGER_HIDDEN + MANAGER_HIDDEN + 1;
pub const SCOUT_WEIGHTS: usize =
    SCOUT_INPUTS.len() * SCOUT_HIDDEN + SCOUT_HIDDEN + SCOUT_HIDDEN + 1;
pub const FINANCE_WEIGHTS: usize =
    FINANCE_INPUTS.len() * FINANCE_HIDDEN + FINANCE_HIDDEN + FINANCE_HIDDEN + 1;

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
        [0.9, 0.2, -0.3, 0.4, 0.1, -0.2, 0.5, 0.0],  // need
        [0.5, 0.6, 0.1, -0.2, 0.3, 0.2, -0.1, 0.4],  // value_fit
        [0.2, 0.1, 0.7, 0.2, -0.1, 0.3, 0.2, -0.2],  // wage_room
        [0.3, -0.1, 0.2, 0.5, 0.4, 0.1, 0.0, 0.3],   // ambition
        [-0.4, 0.2, 0.1, -0.3, 0.5, -0.2, 0.3, 0.1], // rep_gap
        [0.4, 0.3, -0.1, 0.2, 0.1, 0.5, -0.3, 0.2],  // requested
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

    #[test]
    fn specialized_models_have_expected_weight_shapes() {
        assert_eq!(default_player_weights().len(), PLAYER_WEIGHTS);
        assert_eq!(default_manager_weights().len(), MANAGER_WEIGHTS);
        assert_eq!(default_scout_weights().len(), SCOUT_WEIGHTS);
        assert_eq!(default_finance_weights().len(), FINANCE_WEIGHTS);
        let net = player_net(&default_player_weights());
        let inputs = vec![0.1; PLAYER_INPUTS.len()];
        let p = net.forward(&inputs);
        assert!((0.0..=1.0).contains(&p));
        let (p2, audit) = net.audit("contract", "test player", &inputs);
        assert_eq!(p, p2);
        assert_eq!(
            audit.inputs.len(),
            contributions_len(audit.contributions.len())
        );
    }

    #[test]
    fn audit_contains_feature_names_and_consistent_scores() {
        let net = scout_net(&default_scout_weights());
        let inputs = vec![0.2, 1.0, 0.1, 0.4, 0.5, 0.8, 0.9, 0.2, 0.7, 0.3, 0.5, 0.1];
        let (_, audit) = net.audit("scouting", "test prospect", &inputs);
        assert_eq!(audit.inputs.len(), SCOUT_INPUTS.len());
        assert!(audit
            .inputs
            .iter()
            .all(|(name, _)| SCOUT_INPUTS.contains(&name.as_str())));
        assert_eq!(audit.contributions.len(), SCOUT_INPUTS.len());
    }

    #[test]
    fn export_specs_include_models_and_features() {
        let p = crate::tune::Params::default();
        let specs = export_ml_models(&p);
        assert_eq!(specs.len(), 5);
        assert!(specs
            .iter()
            .any(|s| s.model == "player" && s.features.len() == PLAYER_INPUTS.len()));
        assert!(specs
            .iter()
            .any(|s| s.model == "finance" && s.weights.len() == FINANCE_WEIGHTS));
    }

    #[test]
    fn batch_outputs_match_single_outputs_and_export_serializes() {
        let net = player_net(&default_player_weights());
        let examples = vec![
            vec![0.0; PLAYER_INPUTS.len()],
            vec![1.0; PLAYER_INPUTS.len()],
        ];
        let batch = net.forward_batch(&examples);
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0], net.forward(&examples[0]));
        assert_eq!(batch[1], net.forward(&examples[1]));
        let ser = serialize_mlp(&net).unwrap();
        assert_eq!(ser.model, "player");
        assert_eq!(ser.input_weights.len(), PLAYER_INPUTS.len() * PLAYER_HIDDEN);
        assert_eq!(ser.hidden_bias.len(), PLAYER_HIDDEN);
        assert_eq!(ser.output_weights.len(), PLAYER_HIDDEN);
        let (min, max, mean, sd) = output_stats(&batch);
        assert!((0.0..=1.0).contains(&min));
        assert!((0.0..=1.0).contains(&max));
        assert!((0.0..=1.0).contains(&mean));
        assert!((0.0..=1.0).contains(&sd));
    }

    fn contributions_len(n: usize) -> usize {
        n
    }
}
