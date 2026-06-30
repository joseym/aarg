//! The user's career data: the types it is made of (`types`) and the checks
//! that keep it honest (`validate`).
//!
//! Everything aarg produces is assembled from this dataset — it is the
//! evidence base the never-fabricate invariant checks against. Persistence
//! (`store`) lives in the binary crate, which re-exports these types alongside
//! it; the model and its validation are pure and portable, so they live here.

pub mod types;
pub mod validate;

pub use types::{ResumeDataset, SCHEMA_VERSION};
