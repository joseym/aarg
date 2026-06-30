//! The user's career data. The data model (`types`) and its validation
//! (`validate`) live in the portable `aarg-domain` crate; this module adds
//! on-disk persistence (`store`) and re-exports the model so every
//! `crate::dataset::...` path in the binary keeps working unchanged.
//!
//! Everything aarg produces is assembled from this dataset — it is the
//! evidence base the never-fabricate invariant checks against.

pub mod store;

pub use aarg_domain::dataset::{ResumeDataset, SCHEMA_VERSION, types, validate};
pub use store::DatasetError;
