use serde::{Deserialize, Serialize};

use crate::NodeMetrics;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AvecScores {
    pub stability: f64,
    pub logic: f64,
    pub friction: f64,
    pub autonomy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AvecWeights {
    pub formula_version: u32,
    pub stability: StabilityWeights,
    pub logic: LogicWeights,
    pub friction: FrictionWeights,
    pub autonomy: AutonomyWeights,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StabilityWeights {
    pub churn: f64,
    pub contributor: f64,
    pub test: f64,
    pub churn_normalize: f64,
    pub contributor_cap: f64,
    pub test_base_bias: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LogicWeights {
    pub complexity: f64,
    pub parameters: f64,
    pub lines_divisor: f64,
    pub parameter_cap: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FrictionWeights {
    pub structural: f64,
    pub process: f64,
    pub cognitive: f64,
    pub centrality: f64,
    pub dependency: f64,
    pub churn: f64,
    pub collaboration: f64,
    pub incoming_cap: f64,
    pub commits_normalize: f64,
    pub contributors_normalize: f64,
    pub complexity_normalize: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutonomyWeights {
    pub dependency_ratio: f64,
    pub absolute_count: f64,
    pub outgoing_cap: f64,
}

impl AvecWeights {
    pub fn calculate(self, metrics: &NodeMetrics) -> AvecScores {
        AvecScores {
            stability: self.calculate_stability(metrics),
            logic: self.calculate_logic(metrics),
            friction: self.calculate_friction(metrics),
            autonomy: self.calculate_autonomy(metrics),
        }
    }

    fn calculate_stability(self, metrics: &NodeMetrics) -> f64 {
        let average_days = metrics.git_average_days_between_changes.max(1.0);
        let churn_factor = f64::from(metrics.git_total_commits) / average_days;
        let churn_penalty = (churn_factor / self.stability.churn_normalize).min(1.0);
        let contributor_penalty =
            (f64::from(metrics.git_contributors) / self.stability.contributor_cap).min(1.0);
        let test_bonus = f64::midpoint(metrics.test_line_coverage, metrics.test_branch_coverage);

        clamp01(
            (1.0 - churn_penalty * self.stability.churn)
                * (1.0 - contributor_penalty * self.stability.contributor)
                * (self.stability.test_base_bias + test_bonus * self.stability.test),
        )
    }

    fn calculate_logic(self, metrics: &NodeMetrics) -> f64 {
        let normalized_lines =
            (f64::from(metrics.lines_of_code) / self.logic.lines_divisor).max(1.0);
        let complexity_density = f64::from(metrics.cyclomatic_complexity) / normalized_lines;
        let parameter_weight = (f64::from(metrics.parameters) / self.logic.parameter_cap).min(1.0);

        clamp01(
            complexity_density * self.logic.complexity + parameter_weight * self.logic.parameters,
        )
    }

    fn calculate_friction(self, metrics: &NodeMetrics) -> f64 {
        let total_degree = metrics.total_degree().max(1);
        let centrality = f64::from(metrics.incoming_edges) / f64::from(total_degree);
        let dependency_load =
            (f64::from(metrics.incoming_edges) / self.friction.incoming_cap).min(1.0);
        let structural =
            centrality * self.friction.centrality + dependency_load * self.friction.dependency;

        let churn =
            (f64::from(metrics.git_total_commits) / self.friction.commits_normalize).min(1.0);
        let collaboration =
            (f64::from(metrics.git_contributors) / self.friction.contributors_normalize).min(1.0);
        let process = churn * self.friction.churn + collaboration * self.friction.collaboration;

        let cognitive = (f64::from(metrics.cyclomatic_complexity)
            / self.friction.complexity_normalize)
            .min(1.0);

        clamp01(
            structural * self.friction.structural
                + process * self.friction.process
                + cognitive * self.friction.cognitive,
        )
    }

    fn calculate_autonomy(self, metrics: &NodeMetrics) -> f64 {
        let total_degree = metrics.total_degree().max(1);
        let dependency_ratio = f64::from(metrics.outgoing_edges) / f64::from(total_degree);
        let absolute_load =
            (f64::from(metrics.outgoing_edges) / self.autonomy.outgoing_cap).min(1.0);

        clamp01(
            (1.0 - dependency_ratio) * self.autonomy.dependency_ratio
                + (1.0 - absolute_load) * self.autonomy.absolute_count,
        )
    }
}

impl Default for AvecWeights {
    fn default() -> Self {
        Self {
            formula_version: 1,
            stability: StabilityWeights {
                churn: 0.4,
                contributor: 0.3,
                test: 0.3,
                churn_normalize: 10.0,
                contributor_cap: 5.0,
                test_base_bias: 0.5,
            },
            logic: LogicWeights {
                complexity: 0.7,
                parameters: 0.3,
                lines_divisor: 10.0,
                parameter_cap: 5.0,
            },
            friction: FrictionWeights {
                structural: 0.4,
                process: 0.3,
                cognitive: 0.3,
                centrality: 0.4,
                dependency: 0.6,
                churn: 0.7,
                collaboration: 0.3,
                incoming_cap: 10.0,
                commits_normalize: 50.0,
                contributors_normalize: 10.0,
                complexity_normalize: 20.0,
            },
            autonomy: AutonomyWeights {
                dependency_ratio: 0.8,
                absolute_count: 0.2,
                outgoing_cap: 30.0,
            },
        }
    }
}

fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dimension_is_normalized() {
        let metrics = NodeMetrics {
            lines_of_code: 300,
            cyclomatic_complexity: 80,
            parameters: 20,
            incoming_edges: 100,
            outgoing_edges: 100,
            git_total_commits: 1_000,
            git_contributors: 100,
            git_average_days_between_changes: 0.0,
            test_line_coverage: 2.0,
            test_branch_coverage: 2.0,
        };

        let scores = AvecWeights::default().calculate(&metrics);
        for score in [
            scores.stability,
            scores.logic,
            scores.friction,
            scores.autonomy,
        ] {
            assert!((0.0..=1.0).contains(&score));
        }
    }

    #[test]
    fn isolated_code_is_more_autonomous_than_dependency_heavy_code() {
        let weights = AvecWeights::default();
        let isolated = weights.calculate(&NodeMetrics::default());
        let coupled = weights.calculate(&NodeMetrics {
            outgoing_edges: 30,
            incoming_edges: 2,
            ..NodeMetrics::default()
        });

        assert!(isolated.autonomy > coupled.autonomy);
    }
}
