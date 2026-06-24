# AARG demo tape (vhs). Records docs/demo.gif: tailor a mock resume to a mock
# posting, hands-off, through the adversarial loop.
#
# Record with:   vhs docs/demo.tape
#
# Privacy: everything runs under a scratch HOME (/tmp/aarg-demo) with a generic
# prompt, so no username, hostname, or real home path is ever on screen. The
# data is the fictional candidate in docs/demo/. Auth comes from the OS keychain
# (session-scoped, not HOME-scoped), so the scratch HOME still authenticates.

Output docs/demo.gif

Require aarg

Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 800
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

# --- hidden setup: sanitized HOME, clean prompt, self-contained state --------
# Runs from the repo root (where vhs is invoked), so it copies the mock data
# out of docs/demo/ before switching to the scratch HOME.
Hide
Type `export HOME=/tmp/aarg-demo` Enter
Type `export PS1='\[\e[38;5;39m\]aarg\[\e[0m\] \[\e[38;5;245m\]>\[\e[0m\] '` Enter
# Run aarg non-interactively (stdin from /dev/null) so the scripted demo never
# blocks on a copilot prompt, while stderr stays a TTY so streaming still shows.
Type `aarg() { command aarg "$@" < /dev/null; }` Enter
# Fresh sanitized workspace under a scratch HOME: no real path or username ever
# appears on screen, and the real config/keychain registry is untouched.
Type `rm -rf "$HOME"; mkdir -p "$HOME/.config/aarg" "$HOME/work"` Enter
Type `cp docs/demo/resume.md docs/demo/job.txt "$HOME/work/"` Enter
# A config that authenticates via the keychain key labelled "subscription" and
# runs the fast all-Haiku tier. Adjust active_key, or export ANTHROPIC_API_KEY
# before recording, to use your own auth.
Type `printf '%s\n' 'provider = "anthropic"' '[anthropic]' 'model = "claude-haiku-4-5-20251001"' 'keys = ["subscription"]' 'active_key = "subscription"' '[anthropic.tiers]' 'cheap = "claude-haiku-4-5-20251001"' 'mid = "claude-haiku-4-5-20251001"' 'smart = "claude-haiku-4-5-20251001"' '[anthropic.key_kinds]' 'subscription = "oauth"' > "$HOME/.config/aarg/config.toml"` Enter
Type `cd "$HOME/work" && clear` Enter
Show

# --- 1. turn an existing resume into a structured dataset --------------------
Type "aarg ingest resume.md" Enter
Sleep 9s

# --- 2. see what was extracted ----------------------------------------------
Type "aarg dataset show" Enter
Sleep 6s

# --- 3. the adversarial loop: parse, gap, tailor, review, revise, cover -----
# The all-Haiku loop runs ~62-78s depending on whether the score gate stops it
# early; `--cover` drafts a cover letter from the winning draft once the loop
# settles. A fixed sleep covers the full run plus a few seconds to read the
# verdict (a screen-match Wait is unreliable in the headless ttyd terminal).
Type "aarg tailor job.txt --cover" Enter
Sleep 110s

# --- 4. export the finished PDFs (both résumés and the cover letter) --------
Type "aarg export 001 --to out" Enter
Sleep 4s
Type "ls out" Enter
Sleep 5s
