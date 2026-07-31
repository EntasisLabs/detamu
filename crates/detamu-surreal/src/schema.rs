pub const SCHEMA: &str = r"
DEFINE TABLE IF NOT EXISTS detamu_snapshot SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS world_id ON TABLE detamu_snapshot TYPE string;
DEFINE FIELD IF NOT EXISTS snapshot_version ON TABLE detamu_snapshot TYPE string;
DEFINE FIELD IF NOT EXISTS commit_mode ON TABLE detamu_snapshot TYPE string;
DEFINE FIELD IF NOT EXISTS coverage ON TABLE detamu_snapshot TYPE string;
DEFINE FIELD IF NOT EXISTS provenance ON TABLE detamu_snapshot TYPE array<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS diagnostics ON TABLE detamu_snapshot TYPE array<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS entity_count ON TABLE detamu_snapshot TYPE int;
DEFINE FIELD IF NOT EXISTS relation_count ON TABLE detamu_snapshot TYPE int;
DEFINE FIELD IF NOT EXISTS committed_at ON TABLE detamu_snapshot TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS detamu_snapshot_identity ON TABLE detamu_snapshot
    FIELDS world_id, snapshot_version UNIQUE;

DEFINE TABLE IF NOT EXISTS detamu_entity_observation SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS world_id ON TABLE detamu_entity_observation TYPE string;
DEFINE FIELD IF NOT EXISTS snapshot_version ON TABLE detamu_entity_observation TYPE string;
DEFINE FIELD IF NOT EXISTS entity_id ON TABLE detamu_entity_observation TYPE string;
DEFINE FIELD IF NOT EXISTS model_id ON TABLE detamu_entity_observation TYPE string;
DEFINE FIELD IF NOT EXISTS entity_kind ON TABLE detamu_entity_observation TYPE string;
DEFINE FIELD IF NOT EXISTS label ON TABLE detamu_entity_observation TYPE string;
DEFINE FIELD IF NOT EXISTS payload ON TABLE detamu_entity_observation TYPE object FLEXIBLE;
DEFINE INDEX IF NOT EXISTS detamu_entity_snapshot_identity ON TABLE detamu_entity_observation
    FIELDS world_id, snapshot_version, entity_id UNIQUE;
DEFINE INDEX IF NOT EXISTS detamu_entity_model_kind ON TABLE detamu_entity_observation
    FIELDS world_id, snapshot_version, model_id, entity_kind;
DEFINE INDEX IF NOT EXISTS detamu_entity_label ON TABLE detamu_entity_observation
    FIELDS world_id, snapshot_version, label;

DEFINE TABLE IF NOT EXISTS detamu_relation_observation SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS world_id ON TABLE detamu_relation_observation TYPE string;
DEFINE FIELD IF NOT EXISTS snapshot_version ON TABLE detamu_relation_observation TYPE string;
DEFINE FIELD IF NOT EXISTS relation_id ON TABLE detamu_relation_observation TYPE string;
DEFINE FIELD IF NOT EXISTS model_id ON TABLE detamu_relation_observation TYPE string;
DEFINE FIELD IF NOT EXISTS relation_kind ON TABLE detamu_relation_observation TYPE string;
DEFINE FIELD IF NOT EXISTS from_entity_id ON TABLE detamu_relation_observation TYPE string;
DEFINE FIELD IF NOT EXISTS to_entity_id ON TABLE detamu_relation_observation TYPE string;
DEFINE FIELD IF NOT EXISTS weight ON TABLE detamu_relation_observation TYPE float;
DEFINE FIELD IF NOT EXISTS payload ON TABLE detamu_relation_observation TYPE object FLEXIBLE;
DEFINE INDEX IF NOT EXISTS detamu_relation_snapshot_identity ON TABLE detamu_relation_observation
    FIELDS world_id, snapshot_version, relation_id UNIQUE;
DEFINE INDEX IF NOT EXISTS detamu_relation_outgoing ON TABLE detamu_relation_observation
    FIELDS world_id, snapshot_version, from_entity_id;
DEFINE INDEX IF NOT EXISTS detamu_relation_incoming ON TABLE detamu_relation_observation
    FIELDS world_id, snapshot_version, to_entity_id;
DEFINE INDEX IF NOT EXISTS detamu_relation_model_kind ON TABLE detamu_relation_observation
    FIELDS world_id, snapshot_version, model_id, relation_kind;

-- Migrate flexible fields created by older SurrealDB versions. `IF NOT EXISTS`
-- preserves their original definitions, which can leave nested object fields
-- schemafull after upgrading the database engine. These payloads are versioned
-- Detamu contracts and must accept their complete serialized shape.
DEFINE FIELD OVERWRITE provenance ON TABLE detamu_snapshot TYPE array<object> FLEXIBLE;
DEFINE FIELD OVERWRITE diagnostics ON TABLE detamu_snapshot TYPE array<object> FLEXIBLE;
DEFINE FIELD OVERWRITE payload ON TABLE detamu_entity_observation TYPE object FLEXIBLE;
DEFINE FIELD OVERWRITE payload ON TABLE detamu_relation_observation TYPE object FLEXIBLE;
";
