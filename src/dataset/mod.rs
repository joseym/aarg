//! The user's career data: the types it is made of (`types`) and, next,
//! how it is persisted on disk.
//!
//! Everything aarg produces is assembled from this dataset — it is the
//! evidence base the never-fabricate invariant checks against.

pub mod types;

pub use types::{ResumeDataset, SCHEMA_VERSION};
