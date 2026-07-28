use detamu_core::{AnalysisCoverage, ObservationBatch};
use detamu_model::ScoringModel;
use detamu_model_code::{
    AvecCodeScorer, CodeSymbol, DependencyType, GitOid, LanguageId, NodeKind, NodeMetrics,
    RepositoryId, RevisionId, SymbolId, dependency_observation, symbol_observation,
};
use detamu_store::{DetamuStore, InMemoryStore, RelationDirection, SnapshotRecord};
use detamu_surreal::SurrealStore;
use tokio::time::{Duration, sleep};

fn fixture() -> ObservationBatch {
    let revision = RevisionId::new(RepositoryId::new("detamu"), GitOid::new("abc123"));
    let metrics = NodeMetrics {
        lines_of_code: 12,
        cyclomatic_complexity: 2,
        parameters: 1,
        incoming_edges: 0,
        outgoing_edges: 1,
        git_total_commits: 3,
        git_contributors: 1,
        git_average_days_between_changes: 4.0,
        test_line_coverage: 0.8,
        test_branch_coverage: 0.6,
    };
    let source = SymbolId::new("rust:detamu::index");
    let target = SymbolId::new("rust:detamu::store");
    let symbol = |id: SymbolId, name: &str, line: u32| {
        symbol_observation(
            &revision,
            CodeSymbol {
                id,
                language: LanguageId::new("rust"),
                qualified_name: name.to_owned(),
                kind: NodeKind::Function,
            },
            "src/lib.rs",
            line,
            line + 4,
            Some(&format!("fn {name}()")),
            metrics,
        )
    };
    let mut batch = ObservationBatch::empty(revision.snapshot());
    batch.coverage = AnalysisCoverage::Complete;
    batch.entities = vec![
        symbol(source.clone(), "detamu::index", 10),
        symbol(target.clone(), "detamu::store", 30),
    ];
    batch.relations = vec![dependency_observation(
        &revision,
        &source,
        &target,
        &DependencyType::Calls,
        0.7,
    )];
    AvecCodeScorer::default()
        .score(&mut batch)
        .expect("score fixture");
    batch
}

async fn assert_contract(store: &dyn DetamuStore) {
    let batch = fixture();
    let snapshot = batch.snapshot.clone();
    let source = batch.entities[0].entity.id.clone();
    let target = batch.entities[1].entity.id.clone();
    store.ingest(batch.clone()).await.expect("ingest fixture");
    assert_eq!(
        store.snapshot(&snapshot).await.expect("snapshot lookup"),
        Some(SnapshotRecord::from(&batch))
    );
    assert_eq!(
        store
            .snapshots(Some(&snapshot.world))
            .await
            .expect("snapshot enumeration"),
        vec![SnapshotRecord::from(&batch)]
    );
    assert_eq!(
        store.entities(&snapshot).await.expect("entity enumeration"),
        batch.entities
    );
    assert_eq!(
        store
            .snapshot_relations(&snapshot)
            .await
            .expect("relation enumeration"),
        batch.relations
    );
    assert_eq!(
        store
            .entity(&snapshot, &source)
            .await
            .expect("source lookup"),
        Some(batch.entities[0].clone())
    );
    assert_eq!(
        store
            .relations(&snapshot, &source, RelationDirection::Outgoing)
            .await
            .expect("outgoing"),
        batch.relations
    );
    assert_eq!(
        store
            .relations(&snapshot, &target, RelationDirection::Incoming)
            .await
            .expect("incoming")
            .len(),
        1
    );

    let mut replacement = batch;
    replacement.entities.remove(0);
    replacement.relations.clear();
    store.ingest(replacement).await.expect("replace snapshot");
    assert_eq!(
        store
            .entity(&snapshot, &source)
            .await
            .expect("stale lookup"),
        None
    );
    assert_eq!(
        store
            .snapshot(&snapshot)
            .await
            .expect("replacement snapshot")
            .expect("snapshot record")
            .entity_count,
        1
    );
}

#[tokio::test]
async fn memory_and_surreal_implement_the_same_contract() {
    assert_contract(&InMemoryStore::default()).await;
    let surreal = SurrealStore::memory("detamu_test", "contract")
        .await
        .expect("surreal memory")
        .with_write_batch_size(1);
    assert_contract(&surreal).await;
}

#[tokio::test]
async fn rejected_reindex_preserves_the_previous_snapshot() {
    let store = SurrealStore::memory("detamu_test", "rejection")
        .await
        .expect("surreal memory");
    let batch = fixture();
    let snapshot = batch.snapshot.clone();
    let entity = batch.entities[0].entity.id.clone();
    store.ingest(batch.clone()).await.expect("initial ingest");
    let mut invalid = batch.clone();
    invalid.entities.push(batch.entities[0].clone());
    assert!(store.ingest(invalid).await.is_err());
    assert_eq!(
        store.entity(&snapshot, &entity).await.expect("lookup"),
        Some(batch.entities[0].clone())
    );
}

#[tokio::test]
async fn surrealkv_survives_reopen() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let database_path = directory.path().join("detamu.surrealkv");
    let batch = fixture();
    let snapshot = batch.snapshot.clone();
    let entity = batch.entities[0].entity.id.clone();
    {
        let store = SurrealStore::surrealkv(&database_path, "detamu_test", "persistent")
            .await
            .expect("open");
        store
            .ingest(batch.clone())
            .await
            .expect("persistent ingest");
    }
    sleep(Duration::from_millis(250)).await;
    let reopened = SurrealStore::surrealkv(&database_path, "detamu_test", "persistent")
        .await
        .expect("reopen");
    assert_eq!(
        reopened.entity(&snapshot, &entity).await.expect("lookup"),
        Some(batch.entities[0].clone())
    );
}
