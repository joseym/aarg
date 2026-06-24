# AARG evidence-interview demo (vhs). Records docs/demo-evidence.gif: AARG won't
# put a skill on your resume unless you back it with a real role and a real
# sentence. The "machine revises wording, you supply the facts" rule, made
# concrete. Same fictional candidate as the main demo (docs/demo/).
#
# Record with:   vhs docs/demo-evidence.tape
#
# Privacy: everything runs under a scratch HOME (/tmp/aarg-demo-evidence) with a
# generic prompt, so no username, hostname, or real home path is ever on screen.
# The data is the fictional Sam Rivera in docs/demo/, and the answer typed at the
# interview is fictional too. Auth comes from the OS keychain (session-scoped, not
# HOME-scoped), so the scratch HOME still authenticates.

Output docs/demo-evidence.gif

Require aarg

Set Shell "bash"
Set FontSize 16
Set Width 1200
Set Height 800
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

# --- hidden setup: sanitized HOME, clean prompt, a dataset to attach to ------
Hide
Type `export HOME=/tmp/aarg-demo-evidence` Enter
Type `export PS1='\[\e[38;5;39m\]aarg\[\e[0m\] \[\e[38;5;245m\]>\[\e[0m\] '` Enter
Type `rm -rf "$HOME"; mkdir -p "$HOME/.config/aarg" "$HOME/work"` Enter
Type `cp docs/demo/resume.md docs/demo/job.txt "$HOME/work/"` Enter
# Same config as the main demo: keychain key "subscription", all-Haiku tier.
# Adjust active_key, or export ANTHROPIC_API_KEY before recording, for your auth.
Type `printf '%s\n' 'provider = "anthropic"' '[anthropic]' 'model = "claude-haiku-4-5-20251001"' 'keys = ["subscription"]' 'active_key = "subscription"' '[anthropic.tiers]' 'cheap = "claude-haiku-4-5-20251001"' 'mid = "claude-haiku-4-5-20251001"' 'smart = "claude-haiku-4-5-20251001"' '[anthropic.key_kinds]' 'subscription = "oauth"' > "$HOME/.config/aarg/config.toml"` Enter
Type `cd "$HOME/work"` Enter
# A dataset to attach evidence to, built off-screen and non-interactively so the
# GIF opens straight on the interview.
Type `command aarg ingest resume.md < /dev/null > /dev/null 2>&1` Enter
# Wait for the off-screen ingest (a live model call) to finish writing the
# dataset before the visible interview starts — otherwise its keystrokes race
# the still-running ingest and desync.
Sleep 18s
Type `clear` Enter
Show

# --- the never-fabricate rule, made concrete --------------------------------
# The Northwind posting lists Terraform, and Sam's resume never mentioned it.
# Adding it is not a free-text field: AARG makes you point at a real role and
# say what you actually did, and that one sentence becomes the evidence. No
# evidence, no skill on the page.
Type "aarg skills add Terraform --category tool" Enter
Sleep 3.5s

# "Which role best demonstrates Terraform?" -> the current role is first; Enter.
Enter
Sleep 2s

# "Roughly how many years? (blank to skip)" -> skip.
Enter
Sleep 1.5s

# "In a few words, what did you do with Terraform?" -> a real, specific sentence.
Type "Built and own the Terraform modules that provision our three environments"
Sleep 800ms
Enter

# AARG polishes the line into resume wording — a model call — and offers it back
# as a choice ("Use this wording" / "Revise it" / "Keep my own wording").
Sleep 7s
# Accept the suggested wording: "Use this wording" is the highlighted default.
# It's never inflated — the digit-guard rejects any invented number and your own
# words are the floor.
Enter
# The skill is attached as evidence and the dataset saves; the confirmation
# ("recorded Terraform") prints when it lands.
Sleep 6s
