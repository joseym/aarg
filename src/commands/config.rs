//! `aarg config` — show the effective configuration and where it comes
//! from. Read-only: the file is edited by hand or via `aarg init`.

use crate::commands::CliError;
use crate::config::Config;
use crate::secrets;

pub async fn run() -> Result<(), CliError> {
    let path = Config::path()?;
    let config = Config::load()?;
    let file_exists = path.exists();

    // Only presence is reported — the key itself never leaves the
    // keychain for display. An unreachable keychain (e.g. no Secret
    // Service daemon on a headless Linux box) downgrades to a note here
    // instead of failing the whole command: this command is read-only
    // status, and the rest of the config is still worth showing.
    let key_status = match secrets::load_api_key(config.provider.name()).await {
        Ok(Some(_)) => "stored in the OS keychain".to_string(),
        Ok(None) => "not set (run `aarg init`)".to_string(),
        Err(error) => format!("unknown ({error})"),
    };

    println!(
        "config file: {}{}",
        path.display(),
        if file_exists {
            ""
        } else {
            " (not created yet; showing defaults)"
        }
    );
    println!("provider:    {}", config.provider.name());
    println!("model:       {}", config.anthropic.model);
    println!("api key:     {key_status}");
    Ok(())
}
