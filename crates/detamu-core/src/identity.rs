use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }
    };
}

string_id!(WorldId);
string_id!(SnapshotVersion);
string_id!(ModelId);
string_id!(EntityId);
string_id!(RelationId);
string_id!(ScoreModelId);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId {
    pub world: WorldId,
    pub version: SnapshotVersion,
}

impl SnapshotId {
    pub fn new(world: impl Into<WorldId>, version: impl Into<SnapshotVersion>) -> Self {
        Self {
            world: world.into(),
            version: version.into(),
        }
    }
}
