//! Code-world interpretation over Detamu's generic snapshot query facade.

use std::{collections::BTreeSet, sync::Arc};

use detamu_core::{Attributes, EntityId, EntityObservation, SnapshotId};
use detamu_model_code::{AVEC_REQUIRED_MEASUREMENTS, AVEC_SCORE_DIMENSIONS, CODE_MODEL_ID};
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

#[cfg(test)]
mod tests {
    use detamu_core::{
        Entity, Measurement, ModelId, ObservationBatch, Relation, RelationId, RelationObservation,
        Score, ScoreModelId, SnapshotVersion, WorldId,
    };
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
