//! `aarg skills verify` — interview the user about unbacked skills and
//! write the evidence back.
//!
//! Thin glue around `crate::verify`: load, interview, save once. The
//! save only happens when the interview both finished and changed
//! something — an abandoned session leaves the file untouched, which
//! is the transactional half of the feature's promise.

use crate::commands::CliError;
use crate::dataset::store;
use crate::terminal::auto_user;
use crate::verify::verify_unbacked;

pub async fn verify() -> Result<(), CliError> {
    let mut dataset = store::load()?;
    let user = auto_user();

    let outcome = verify_unbacked(&mut dataset, user.as_ref()).await?;

    if outcome.changed() {
        dataset.metadata.updated_at = chrono::Utc::now();
        store::save(&dataset)?;
    }
    println!(
        "verified {} · removed {} · skipped {} · bullets added {}{}",
        outcome.verified,
        outcome.removed,
        outcome.skipped,
        outcome.bullets_added,
        if outcome.changed() {
            " · dataset saved (previous version backed up)"
        } else {
            " · dataset unchanged"
        }
    );
    Ok(())
}
