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

use std::io::IsTerminal;

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::Cli;
use crate::commands::CliError;
use crate::style;

pub fn run(shell: Shell) -> Result<(), CliError> {
    let mut command = Cli::command();
    let name = command.get_name().to_string();
    // Generator writes the script straight to stdout; nothing to collect.
    generate(shell, &mut command, name, &mut std::io::stdout());

    // If the script went to a terminal rather than a pipe or a file, the user
    // almost certainly ran the command to see what to do with it — a wall of
    // completion script just scrolled past. Tell them the one line that
    // installs it. When stdout is redirected (`> file`) or substituted
    // (`source <(...)`), they know what they're doing, so stay silent and keep
    // the captured output pure. The hint goes to stderr regardless, so it never
    // corrupts the script even if this check is ever wrong.
    if std::io::stdout().is_terminal() {
        eprintln!();
        eprintln!(
            "{}",
            style::info("that was the completion script. to install it:")
        );
        eprintln!("  {}", install_hint(shell));
    }
    Ok(())
}

/// The one command that installs completions for `shell`. bash/zsh use process
/// substitution so the script regenerates (and stays current) on every shell
/// start; fish and PowerShell read it from a file in their completion dir.
fn install_hint(shell: Shell) -> String {
    match shell {
        Shell::Bash => "echo 'source <(aarg completions bash)' >> ~/.bashrc".into(),
        Shell::Zsh => "echo 'source <(aarg completions zsh)' >> ~/.zshrc".into(),
        Shell::Fish => "aarg completions fish > ~/.config/fish/completions/aarg.fish".into(),
        Shell::PowerShell => "aarg completions powershell >> $PROFILE".into(),
        // Shell is non-exhaustive; a generic line still points the right way.
        other => format!("redirect `aarg completions {other}` into your shell's completion file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_hint_is_shell_specific_and_runnable() {
        // bash/zsh: the process-substitution line the testers landed on.
        assert_eq!(
            install_hint(Shell::Zsh),
            "echo 'source <(aarg completions zsh)' >> ~/.zshrc"
        );
        assert!(install_hint(Shell::Bash).contains("source <(aarg completions bash)"));
        // fish/powershell: file- and profile-based, not process substitution.
        assert!(install_hint(Shell::Fish).contains("completions/aarg.fish"));
        assert!(install_hint(Shell::PowerShell).contains("$PROFILE"));
    }
}
