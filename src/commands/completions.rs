//! `aarg completions <shell>` — print a shell completion script.
//!
//! The script is generated from the same `clap` command tree the binary
//! parses, so it always matches the real commands, subcommands, and flags —
//! there is no hand-maintained list to drift. Output goes to stdout; the
//! user installs it the way their shell expects, e.g.
//!
//! ```text
//! aarg completions bash > ~/.local/share/bash-completion/completions/aarg
//! aarg completions zsh  > ~/.zfunc/_aarg        # with ~/.zfunc on $fpath
//! aarg completions fish > ~/.config/fish/completions/aarg.fish
//! ```

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::Cli;
use crate::commands::CliError;

pub fn run(shell: Shell) -> Result<(), CliError> {
    let mut command = Cli::command();
    let name = command.get_name().to_string();
    // Generator writes the script straight to stdout; nothing to collect.
    generate(shell, &mut command, name, &mut std::io::stdout());
    Ok(())
}
