//! The user's career data: the types it is made of (`types`) and how it
//! is persisted on disk (`store`).
//!
//! Everything aarg produces is assembled from this dataset — it is the
//! evidence base the never-fabricate invariant checks against.

pub mod store;
pub mod types;
pub mod validate;

pub use store::DatasetError;
pub use types::{ResumeDataset, SCHEMA_VERSION};
