//! Code-world interpretation over Detamu's generic snapshot query facade.

use std::{collections::BTreeSet, sync::Arc};

use detamu_core::{Attributes, EntityId, EntityObservation, RelationObservation, SnapshotId};
use detamu_model_code::{
    AVEC_REQUIRED_MEASUREMENTS, AVEC_SCORE_DIMENSIONS, AvecScores, CODE_MODEL_ID,
};
use detamu_query::{
    EntityFilter, GraphRequest, GraphTraversal, QUERY_SCHEMA_VERSION, QueryError, SnapshotQuery,
};
use detamu_store::{DetamuStore, RelationDirection, SnapshotRecord};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeEntityFilter {
    pub path: Option<String>,
    pub name_contains: Option<String>,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeImpact {
    pub schema_version: u32,
    pub snapshot: SnapshotId,
    pub target: EntityObservation,
    pub direct_dependents: usize,
    pub transitive_dependents: usize,
    pub graph: GraphTraversal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeEntitySummary {
    pub id: EntityId,
    pub label: String,
    pub kind: String,
    pub path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
}

impl From<&EntityObservation> for CodeEntitySummary {
    fn from(observation: &EntityObservation) -> Self {
        Self {
            id: observation.entity.id.clone(),
            label: observation.entity.label.clone(),
            kind: observation.entity.kind.clone(),
            path: observation
                .attributes
                .get("file_path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            line_start: u32_attribute(observation, "line_start"),
            line_end: u32_attribute(observation, "line_end"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityAnalysisGap {
    pub entity: CodeEntitySummary,
    pub missing_measurements: Vec<String>,
    pub missing_scores: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisGapReport {
    pub schema_version: u32,
    pub snapshot: SnapshotId,
    pub metadata: Option<SnapshotRecord>,
    pub scoreable_entities: usize,
    pub fully_scored_entities: usize,
    pub gaps: Vec<EntityAnalysisGap>,
}

pub const PATTERN_THRESHOLD: f64 = 0.8;
pub const PATTERN_LIMIT: usize = 50;
pub const FRICTION_MINIMUM: f64 = 0.7;
pub const UNSTABLE_MAXIMUM: f64 = 0.4;
pub const RANK_LIMIT: usize = 20;
const SCORE_MODEL: &str = "avec.code";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeEdge {
    pub kind: String,
    pub weight: f64,
    pub entity: EntityId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeNode {
    pub schema_version: u32,
    pub id: EntityId,
    pub kind: String,
    pub language: Option<String>,
    pub name: String,
    pub namespace: Option<String>,
    pub signature: Option<String>,
    pub file_path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub incoming_edges: Option<u32>,
    pub outgoing_edges: Option<u32>,
    pub avec: Option<AvecScores>,
    pub incoming: Vec<CodeEdge>,
    pub outgoing: Vec<CodeEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeDependencies {
    pub schema_version: u32,
    pub snapshot: SnapshotId,
    pub root: EntityId,
    pub direction: RelationDirection,
    pub nodes: Vec<DependencyHit>,
    pub relations: Vec<RelationObservation>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DependencyHit {
    pub depth: u32,
    pub node: CodeNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternMatch {
    pub distance: f64,
    pub node: CodeNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectStats {
    pub schema_version: u32,
    pub snapshot: SnapshotId,
    pub code_entities: usize,
    pub scored_entities: usize,
    pub mean_stability: Option<f64>,
    pub mean_logic: Option<f64>,
    pub mean_friction: Option<f64>,
    pub mean_autonomy: Option<f64>,
}

pub struct CodeQuery {
    query: SnapshotQuery,
}

impl CodeQuery {
    pub fn new(store: Arc<dyn DetamuStore>) -> Self {
        Self {
            query: SnapshotQuery::new(store),
        }
    }

    /// Finds code entities by code-domain conveniences.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot enumerate the snapshot.
    pub async fn find(
        &self,
        snapshot: &SnapshotId,
        filter: &CodeEntityFilter,
    ) -> Result<Vec<EntityObservation>, QueryError> {
        let mut attributes = Attributes::new();
        if let Some(path) = &filter.path {
            attributes.insert("file_path".to_owned(), json!(path));
        }
        if let Some(language) = &filter.language {
            attributes.insert("language".to_owned(), json!(language));
        }
        self.query
            .find_entities(
                snapshot,
                &EntityFilter {
                    model: Some(CODE_MODEL_ID.to_owned()),
                    kind: filter.kind.clone(),
                    label_contains: filter.name_contains.clone(),
                    attributes,
                    limit: filter.limit,
                },
            )
            .await
    }

    /// Finds the narrowest code entity containing a one-based source line.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot enumerate the snapshot.
    pub async fn at_location(
        &self,
        snapshot: &SnapshotId,
        path: &str,
        line: u32,
    ) -> Result<Option<EntityObservation>, QueryError> {
        let mut matches = self
            .find(
                snapshot,
                &CodeEntityFilter {
                    path: Some(path.to_owned()),
                    ..CodeEntityFilter::default()
                },
            )
            .await?
            .into_iter()
            .filter_map(|observation| {
                let start = u32_attribute(&observation, "line_start")?;
                let end = u32_attribute(&observation, "line_end").unwrap_or(start);
                (start <= line && line <= end).then_some((end.saturating_sub(start), observation))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.entity.id.as_str().cmp(right.1.entity.id.as_str()))
        });
        Ok(matches.into_iter().next().map(|(_, entity)| entity))
    }

    /// Traverses callers, references, imports, and type dependencies that can
    /// be affected by changing one code entity.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is absent, bounds are invalid, or the
    /// backing store cannot enumerate the snapshot.
    pub async fn impact(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
        max_depth: u32,
        max_nodes: usize,
    ) -> Result<CodeImpact, QueryError> {
        let graph = self
            .query
            .traverse(
                snapshot,
                &GraphRequest {
                    root: entity.clone(),
                    direction: RelationDirection::Incoming,
                    max_depth,
                    max_nodes,
                    relation_kinds: impact_relation_kinds(),
                },
            )
            .await?;
        let target = graph
            .nodes
            .iter()
            .find(|node| node.depth == 0)
            .map(|node| node.observation.clone())
            .ok_or_else(|| QueryError::EntityNotFound {
                entity: entity.clone(),
            })?;
        let direct_dependents = graph.nodes.iter().filter(|node| node.depth == 1).count();
        let transitive_dependents = graph.nodes.iter().filter(|node| node.depth > 1).count();
        Ok(CodeImpact {
            schema_version: QUERY_SCHEMA_VERSION,
            snapshot: snapshot.clone(),
            target,
            direct_dependents,
            transitive_dependents,
            graph,
        })
    }

    /// Reports why code entities cannot yet receive complete AVEC output.
    ///
    /// # Errors
    ///
    /// Returns an error when snapshot metadata or entities cannot be read.
    pub async fn gaps(&self, snapshot: &SnapshotId) -> Result<AnalysisGapReport, QueryError> {
        let metadata = self.query.snapshot(snapshot).await?;
        let entities = self
            .query
            .find_entities(
                snapshot,
                &EntityFilter {
                    model: Some(CODE_MODEL_ID.to_owned()),
                    ..EntityFilter::default()
                },
            )
            .await?;
        let mut scoreable_entities = 0;
        let mut fully_scored_entities = 0;
        let mut gaps = Vec::new();
        for entity in entities {
            if !entity
                .measurements
                .iter()
                .any(|measurement| measurement.name == "code.lines_of_code")
            {
                continue;
            }
            scoreable_entities += 1;
            let missing_measurements = AVEC_REQUIRED_MEASUREMENTS
                .iter()
                .filter(|name| {
                    !entity
                        .measurements
                        .iter()
                        .any(|measurement| measurement.name == **name)
                })
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>();
            let missing_scores = AVEC_SCORE_DIMENSIONS
                .iter()
                .filter(|dimension| {
                    !entity
                        .scores
                        .iter()
                        .any(|score| score.dimension == **dimension)
                })
                .map(|dimension| (*dimension).to_owned())
                .collect::<Vec<_>>();
            if missing_measurements.is_empty() && missing_scores.is_empty() {
                fully_scored_entities += 1;
            } else {
                gaps.push(EntityAnalysisGap {
                    entity: CodeEntitySummary::from(&entity),
                    missing_measurements,
                    missing_scores,
                });
            }
        }
        Ok(AnalysisGapReport {
            schema_version: QUERY_SCHEMA_VERSION,
            snapshot: snapshot.clone(),
            metadata,
            scoreable_entities,
            fully_scored_entities,
            gaps,
        })
    }

    pub fn generic(&self) -> &SnapshotQuery {
        &self.query
    }

    /// Builds the query view of one entity without loading its edges.
    #[must_use]
    pub fn describe(observation: &EntityObservation, include_scores: bool) -> CodeNode {
        code_node(observation, &[], include_scores)
    }

    /// Loads one code entity with its immediate dependency edges.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot read the snapshot.
    pub async fn node(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
        include_scores: bool,
    ) -> Result<Option<CodeNode>, QueryError> {
        let Some(observation) = self.query.entity(snapshot, entity).await? else {
            return Ok(None);
        };
        let relations = self
            .query
            .store()
            .relations(snapshot, entity, RelationDirection::Both)
            .await?;
        Ok(Some(code_node(&observation, &relations, include_scores)))
    }

    /// Traverses dependency edges in the requested direction.
    ///
    /// `max_depth` of `u32::MAX` walks until `max_nodes` stops the search.
    /// Containment edges are omitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the root is absent, the node limit is zero, or
    /// the backing store cannot read the snapshot.
    pub async fn dependencies(
        &self,
        snapshot: &SnapshotId,
        entity: &EntityId,
        direction: RelationDirection,
        max_depth: u32,
        max_nodes: usize,
        include_scores: bool,
    ) -> Result<CodeDependencies, QueryError> {
        let graph = self
            .query
            .traverse(
                snapshot,
                &GraphRequest {
                    root: entity.clone(),
                    direction,
                    max_depth,
                    max_nodes,
                    relation_kinds: impact_relation_kinds(),
                },
            )
            .await?;
        let nodes = graph
            .nodes
            .iter()
            .map(|node| DependencyHit {
                depth: node.depth,
                node: code_node(&node.observation, &[], include_scores),
            })
            .collect();
        Ok(CodeDependencies {
            schema_version: QUERY_SCHEMA_VERSION,
            snapshot: snapshot.clone(),
            root: entity.clone(),
            direction,
            nodes,
            relations: graph.relations,
            truncated: graph.truncated,
        })
    }

    /// Finds scored symbols near a target AVEC profile.
    ///
    /// Distance is Euclidean distance in the four AVEC dimensions. A threshold
    /// of 0.8 keeps nodes whose distance is at most 0.2, matching ACC.
    ///
    /// # Errors
    ///
    /// Returns an error when the threshold is outside 0..=1, the limit is zero,
    /// or the backing store cannot enumerate the snapshot.
    pub async fn patterns(
        &self,
        snapshot: &SnapshotId,
        target: AvecScores,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<PatternMatch>, QueryError> {
        if !(0.0..=1.0).contains(&threshold) {
            return Err(QueryError::InvalidRequest(
                "similarity threshold must be between 0 and 1".to_owned(),
            ));
        }
        if limit == 0 {
            return Err(QueryError::InvalidRequest(
                "pattern limit must be greater than zero".to_owned(),
            ));
        }
        let max_distance = 1.0 - threshold;
        let mut matches = code_entities(self, snapshot)
            .await?
            .into_iter()
            .filter_map(|observation| {
                let profile = avec_scores(&observation)?;
                let distance = avec_distance(profile, target);
                (distance <= max_distance).then_some(PatternMatch {
                    distance,
                    node: code_node(&observation, &[], true),
                })
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            left.distance
                .total_cmp(&right.distance)
                .then_with(|| left.node.id.as_str().cmp(right.node.id.as_str()))
        });
        matches.truncate(limit);
        Ok(matches)
    }

    /// Returns symbols whose friction is at least `minimum`, highest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the limit is zero or the snapshot cannot be read.
    pub async fn high_friction(
        &self,
        snapshot: &SnapshotId,
        minimum: f64,
        limit: usize,
    ) -> Result<Vec<CodeNode>, QueryError> {
        ranked(
            self,
            snapshot,
            limit,
            move |scores| scores.friction >= minimum,
            |left, right| {
                score_dimension(right, |scores| scores.friction)
                    .total_cmp(&score_dimension(left, |scores| scores.friction))
            },
        )
        .await
    }

    /// Returns symbols whose stability is at most `maximum`, lowest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the limit is zero or the snapshot cannot be read.
    pub async fn unstable(
        &self,
        snapshot: &SnapshotId,
        maximum: f64,
        limit: usize,
    ) -> Result<Vec<CodeNode>, QueryError> {
        ranked(
            self,
            snapshot,
            limit,
            move |scores| scores.stability <= maximum,
            |left, right| {
                score_dimension(left, |scores| scores.stability)
                    .total_cmp(&score_dimension(right, |scores| scores.stability))
            },
        )
        .await
    }

    /// Summarizes how many code entities have AVEC scores and their means.
    ///
    /// Means are absent when nothing has been scored. They are not zero.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be read.
    pub async fn stats(&self, snapshot: &SnapshotId) -> Result<ProjectStats, QueryError> {
        let entities = code_entities(self, snapshot).await?;
        let scored = entities.iter().filter_map(avec_scores).collect::<Vec<_>>();
        let mean = |select: fn(AvecScores) -> f64| {
            let count = u32::try_from(scored.len()).unwrap_or(u32::MAX);
            (count > 0).then(|| {
                scored.iter().map(|scores| select(*scores)).sum::<f64>() / f64::from(count)
            })
        };
        Ok(ProjectStats {
            schema_version: QUERY_SCHEMA_VERSION,
            snapshot: snapshot.clone(),
            code_entities: entities.len(),
            scored_entities: scored.len(),
            mean_stability: mean(|scores| scores.stability),
            mean_logic: mean(|scores| scores.logic),
            mean_friction: mean(|scores| scores.friction),
            mean_autonomy: mean(|scores| scores.autonomy),
        })
    }
}

fn impact_relation_kinds() -> BTreeSet<String> {
    ["calls", "references", "imports", "implements", "inherits"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn u32_attribute(observation: &EntityObservation, name: &str) -> Option<u32> {
    u32::try_from(observation.attributes.get(name)?.as_u64()?).ok()
}

async fn code_entities(
    query: &CodeQuery,
    snapshot: &SnapshotId,
) -> Result<Vec<EntityObservation>, QueryError> {
    query.find(snapshot, &CodeEntityFilter::default()).await
}

async fn ranked(
    query: &CodeQuery,
    snapshot: &SnapshotId,
    limit: usize,
    accept: impl Fn(AvecScores) -> bool,
    order: impl Fn(&CodeNode, &CodeNode) -> std::cmp::Ordering,
) -> Result<Vec<CodeNode>, QueryError> {
    if limit == 0 {
        return Err(QueryError::InvalidRequest(
            "result limit must be greater than zero".to_owned(),
        ));
    }
    let mut nodes = code_entities(query, snapshot)
        .await?
        .into_iter()
        .filter_map(|observation| {
            let scores = avec_scores(&observation)?;
            accept(scores).then(|| code_node(&observation, &[], true))
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        order(left, right).then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    nodes.truncate(limit);
    Ok(nodes)
}

fn code_node(
    observation: &EntityObservation,
    relations: &[RelationObservation],
    include_scores: bool,
) -> CodeNode {
    let mut incoming = Vec::new();
    let mut outgoing = Vec::new();
    for relation in relations {
        if relation.relation.kind == "contains" {
            continue;
        }
        let from_self = relation.relation.from == observation.entity.id;
        let to_self = relation.relation.to == observation.entity.id;
        if !from_self && !to_self {
            continue;
        }
        let edge = CodeEdge {
            kind: relation.relation.kind.clone(),
            weight: relation.weight,
            entity: if from_self {
                relation.relation.to.clone()
            } else {
                relation.relation.from.clone()
            },
        };
        if from_self {
            outgoing.push(edge);
        } else {
            incoming.push(edge);
        }
    }
    CodeNode {
        schema_version: QUERY_SCHEMA_VERSION,
        id: observation.entity.id.clone(),
        kind: observation.entity.kind.clone(),
        language: string_attribute(observation, "language"),
        name: observation.entity.label.clone(),
        namespace: string_attribute(observation, "namespace"),
        signature: string_attribute(observation, "signature"),
        file_path: string_attribute(observation, "file_path"),
        line_start: u32_attribute(observation, "line_start"),
        line_end: u32_attribute(observation, "line_end"),
        incoming_edges: count_measurement(observation, "graph.incoming_edges"),
        outgoing_edges: count_measurement(observation, "graph.outgoing_edges"),
        avec: include_scores.then(|| avec_scores(observation)).flatten(),
        incoming,
        outgoing,
    }
}

fn string_attribute(observation: &EntityObservation, name: &str) -> Option<String> {
    observation
        .attributes
        .get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn count_measurement(observation: &EntityObservation, name: &str) -> Option<u32> {
    let value = observation
        .measurements
        .iter()
        .find(|measurement| measurement.name == name)?
        .value;
    (value.is_finite() && value >= 0.0 && value <= f64::from(u32::MAX) && value.fract() == 0.0)
        .then_some(value as u32)
}

fn avec_scores(observation: &EntityObservation) -> Option<AvecScores> {
    Some(AvecScores {
        stability: score_value(observation, "stability")?,
        logic: score_value(observation, "logic")?,
        friction: score_value(observation, "friction")?,
        autonomy: score_value(observation, "autonomy")?,
    })
}

fn score_value(observation: &EntityObservation, dimension: &str) -> Option<f64> {
    observation
        .scores
        .iter()
        .find(|score| score.model.as_str() == SCORE_MODEL && score.dimension == dimension)
        .map(|score| score.value)
}

fn score_dimension(node: &CodeNode, select: impl Fn(AvecScores) -> f64) -> f64 {
    node.avec.map_or(0.0, select)
}

fn avec_distance(left: AvecScores, right: AvecScores) -> f64 {
    let deltas = [
        left.stability - right.stability,
        left.logic - right.logic,
        left.friction - right.friction,
        left.autonomy - right.autonomy,
    ];
    deltas.iter().map(|delta| delta * delta).sum::<f64>().sqrt()
}

#[cfg(test)]
mod tests {
    use detamu_core::{
        Entity, Measurement, ModelId, ObservationBatch, Relation, RelationId, RelationObservation,
        Score, ScoreModelId, SnapshotVersion, WorldId,
    };
    use detamu_model_code::AvecScores;
    use detamu_store::{DetamuStore, InMemoryStore};

    use super::*;

    #[tokio::test]
    async fn location_lookup_prefers_the_narrowest_symbol() {
        let (store, snapshot) = fixture_store().await;
        let query = CodeQuery::new(store);

        let entity = query
            .at_location(&snapshot, "src/lib.rs", 12)
            .await
            .expect("lookup location")
            .expect("matching symbol");

        assert_eq!(entity.entity.id.as_str(), "target");
    }

    #[tokio::test]
    async fn impact_walks_reverse_code_dependencies() {
        let (store, snapshot) = fixture_store().await;
        let query = CodeQuery::new(store);

        let impact = query
            .impact(&snapshot, &EntityId::new("target"), 3, 100)
            .await
            .expect("impact analysis");

        assert_eq!(impact.direct_dependents, 1);
        assert_eq!(impact.transitive_dependents, 1);
        assert_eq!(impact.graph.relations.len(), 2);
    }

    #[tokio::test]
    async fn gaps_explain_missing_avec_inputs_and_scores() {
        let (store, snapshot) = fixture_store().await;
        let query = CodeQuery::new(store);

        let report = query.gaps(&snapshot).await.expect("gap report");

        assert_eq!(report.scoreable_entities, 2);
        assert_eq!(report.fully_scored_entities, 1);
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].entity.id.as_str(), "target");
        assert!(
            report.gaps[0]
                .missing_measurements
                .contains(&"test.line_coverage".to_owned())
        );
        assert_eq!(report.gaps[0].missing_scores.len(), 4);
    }

    #[tokio::test]
    async fn rankings_follow_avec_distance_friction_and_stability() {
        let store = Arc::new(InMemoryStore::default());
        let snapshot = SnapshotId::new(
            WorldId::new("code.repository:fixture"),
            SnapshotVersion::new("v1"),
        );
        let mut calm = entity(&snapshot, "calm", "calm", 1, 2);
        let mut hot = entity(&snapshot, "hot", "hot", 3, 4);
        calm.scores = profile_scores(0.9, 0.2, 0.1, 0.8);
        hot.scores = profile_scores(0.2, 0.2, 0.9, 0.8);
        let mut batch = ObservationBatch::empty(snapshot.clone());
        batch.entities = vec![calm, hot];
        store.ingest(batch).await.expect("ingest");
        let query = CodeQuery::new(store);

        let patterns = query
            .patterns(
                &snapshot,
                AvecScores {
                    stability: 0.9,
                    logic: 0.2,
                    friction: 0.1,
                    autonomy: 0.8,
                },
                0.8,
                50,
            )
            .await
            .expect("patterns");
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].node.id.as_str(), "calm");

        let friction = query
            .high_friction(&snapshot, 0.7, 20)
            .await
            .expect("friction");
        assert_eq!(friction[0].id.as_str(), "hot");

        let unstable = query.unstable(&snapshot, 0.4, 20).await.expect("unstable");
        assert_eq!(unstable[0].id.as_str(), "hot");

        let stats = query.stats(&snapshot).await.expect("stats");
        assert_eq!(stats.scored_entities, 2);
        assert!((stats.mean_stability.expect("mean") - 0.55).abs() < 1e-9);
    }

    fn profile_scores(stability: f64, logic: f64, friction: f64, autonomy: f64) -> Vec<Score> {
        [
            ("stability", stability),
            ("logic", logic),
            ("friction", friction),
            ("autonomy", autonomy),
        ]
        .into_iter()
        .map(|(dimension, value)| Score {
            model: ScoreModelId::new("avec.code"),
            version: 1,
            dimension: dimension.to_owned(),
            value,
        })
        .collect()
    }

    async fn fixture_store() -> (Arc<InMemoryStore>, SnapshotId) {
        let store = Arc::new(InMemoryStore::default());
        let snapshot = SnapshotId::new(
            WorldId::new("code.repository:fixture"),
            SnapshotVersion::new("v1"),
        );
        let mut batch = ObservationBatch::empty(snapshot.clone());
        let mut target = entity(&snapshot, "target", "target", 10, 14);
        target.measurements.push(measurement("code.lines_of_code"));
        let mut caller = entity(&snapshot, "caller", "caller", 1, 30);
        caller.measurements = AVEC_REQUIRED_MEASUREMENTS
            .iter()
            .map(|name| measurement(name))
            .collect();
        caller.scores = AVEC_SCORE_DIMENSIONS
            .iter()
            .map(|dimension| Score {
                model: ScoreModelId::new("avec-code"),
                version: 1,
                dimension: (*dimension).to_owned(),
                value: 0.5,
            })
            .collect();
        batch.entities = vec![
            target,
            caller,
            entity(&snapshot, "transitive", "transitive", 40, 45),
        ];
        batch.relations = vec![
            relation(&snapshot, "caller", "target"),
            relation(&snapshot, "transitive", "caller"),
        ];
        store.ingest(batch).await.expect("ingest fixture");
        (store, snapshot)
    }

    fn entity(
        snapshot: &SnapshotId,
        id: &str,
        label: &str,
        start: u32,
        end: u32,
    ) -> EntityObservation {
        let mut attributes = Attributes::new();
        attributes.insert("file_path".to_owned(), json!("src/lib.rs"));
        attributes.insert("language".to_owned(), json!("rust"));
        attributes.insert("line_start".to_owned(), json!(start));
        attributes.insert("line_end".to_owned(), json!(end));
        EntityObservation {
            snapshot: snapshot.clone(),
            entity: Entity {
                id: EntityId::new(id),
                model: ModelId::new(CODE_MODEL_ID),
                kind: "function".to_owned(),
                label: label.to_owned(),
            },
            attributes,
            measurements: Vec::new(),
            scores: Vec::new(),
        }
    }

    fn measurement(name: &str) -> Measurement {
        Measurement {
            name: name.to_owned(),
            value: 1.0,
            unit: None,
            evidence: None,
        }
    }

    fn relation(snapshot: &SnapshotId, from: &str, to: &str) -> RelationObservation {
        RelationObservation {
            snapshot: snapshot.clone(),
            relation: Relation {
                id: RelationId::new(format!("{from}:calls:{to}")),
                model: ModelId::new(CODE_MODEL_ID),
                kind: "calls".to_owned(),
                from: EntityId::new(from),
                to: EntityId::new(to),
            },
            weight: 1.0,
            attributes: Attributes::new(),
        }
    }
}
