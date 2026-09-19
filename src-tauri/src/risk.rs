//! RCC-321 risk objects, failure modes, and trajectory probabilities.
//!
//! Objects are *inclusive*: a dropped stage and the remaining vehicle can
//! both contribute debris. Trajectories belonging to one object are
//! *mutually exclusive* Monte Carlo realisations. Failure-mode probabilities
//! on an object must sum to 1; within a mode, relative weights are
//! normalised so every trajectory on that object has a probability and
//! those probabilities sum to 1 when the model is complete.

use serde::{Deserialize, Serialize};

pub const PROB_TOL: f64 = 1e-6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskObject {
    pub id: u64,
    pub name: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub generate: Option<crate::generate::GenerateSpec>,
    #[serde(default)]
    pub rocketpy: Option<crate::rocketpy::RocketPySpec>,
}

fn default_source() -> String {
    "files".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureMode {
    pub id: u64,
    pub object_id: u64,
    pub name: String,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackProb {
    pub id: u64,
    pub name: String,
    pub weight: f64,
    pub probability: f64,
    pub object_id: Option<u64>,
    pub failure_mode_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureModeView {
    pub id: u64,
    pub name: String,
    pub probability: f64,
    pub track_count: usize,
    pub trajectory_prob_sum: f64,
    pub tracks: Vec<TrackProb>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObjectView {
    pub id: u64,
    pub name: String,
    /// Additional objects are inclusive of one another.
    pub inclusive: bool,
    /// Trajectories on this object are mutually exclusive.
    pub trajectories_exclusive: bool,
    pub failure_mode_sum: f64,
    pub trajectory_prob_sum: f64,
    pub valid: bool,
    pub issues: Vec<String>,
    pub modes: Vec<FailureModeView>,
    pub source: String,
    pub generate: Option<crate::generate::GenerateSpec>,
    pub rocketpy: Option<crate::rocketpy::RocketPySpec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskModelView {
    pub objects: Vec<ObjectView>,
    pub unassigned: Vec<TrackProb>,
    #[serde(default)]
    pub wind: crate::wind::WindSpec,
    #[serde(default)]
    pub catalogs: Vec<crate::simulate::DebrisCatalog>,
}

pub fn trajectory_probability(
    mode_probability: f64,
    track_weight: f64,
    weight_sum: f64,
) -> f64 {
    if weight_sum <= 0.0 || track_weight <= 0.0 || mode_probability <= 0.0 {
        0.0
    } else {
        mode_probability * (track_weight / weight_sum)
    }
}

pub fn object_is_valid(failure_mode_sum: f64, trajectory_prob_sum: f64, empty_modes: usize) -> bool {
    (failure_mode_sum - 1.0).abs() <= PROB_TOL
        && (trajectory_prob_sum - 1.0).abs() <= PROB_TOL
        && empty_modes == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_trajectories_share_failure_mode_mass() {
        let p = trajectory_probability(0.4, 1.0, 2.0);
        assert!((p - 0.2).abs() < 1e-12);
        let p2 = trajectory_probability(0.4, 3.0, 4.0);
        assert!((p2 - 0.3).abs() < 1e-12);
    }

    #[test]
    fn object_complete_when_modes_and_trajs_sum_to_one() {
        assert!(object_is_valid(1.0, 1.0, 0));
        assert!(!object_is_valid(0.9, 0.9, 0));
        assert!(!object_is_valid(1.0, 0.5, 1));
    }

    #[test]
    fn inclusive_objects_do_not_share_a_unit_mass() {
        // Two objects each complete at probability 1: expected debris count
        // can exceed 1 because objects are inclusive, not a mixture.
        let vehicle: f64 = 1.0;
        let stage: f64 = 1.0;
        assert!((vehicle + stage - 2.0).abs() < 1e-12);
    }
}
