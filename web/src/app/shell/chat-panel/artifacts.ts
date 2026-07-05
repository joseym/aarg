/** The chat panel's artifact layer: the fixed set of documents a card can show,
 *  the slash-command registry that inserts one on demand, the marker convention
 *  the model uses to attach one from prose, and the pure text projections a card
 *  copies and downloads.
 *
 *  Every card's content is RETRIEVED, never generated: the job posting and the
 *  resume come straight from the open build's stored context, the cover letter
 *  from its stored payload. The model's only role is naming which artifact to
 *  attach (the {@link ArtifactKind} enum below), never producing its content. */

/** The three documents a card can surface. A fixed enum on purpose: the model
 *  may only name one of these, and an unknown name renders nothing. */
export type ArtifactKind = 'job_description' | 'resume' | 'cover_letter';

/** The enum values, for validating a marker the model emitted. */
const KNOWN_ARTIFACTS: readonly ArtifactKind[] = [
  'job_description',
  'resume',
  'cover_letter',
];

// ── slash-command registry ────────────────────────────────────────────────

/** What running a slash command does. Today the only outcome is attaching an
 *  artifact card, inserted client-side with no model call. A later `/skills`
 *  wave adds its own variants to this union and the panel grows one branch to
 *  apply them, so the trigger surface stays a single registry rather than a
 *  chain of hardcoded `if`s. */
export type SlashOutcome = { type: 'artifact'; artifact: ArtifactKind };

/** One registered slash command. `run` is pure — it returns the outcome and the
 *  panel applies it — so the registry has no dependency on the panel's state. */
export interface SlashCommand {
  /** The word after the slash, e.g. `jd`. Lower-case, no spaces. */
  readonly name: string;
  /** Alternate spellings that resolve to the same command. */
  readonly aliases?: readonly string[];
  /** One line describing the command, shown in the autocomplete hint. */
  readonly hint: string;
  /** Produce the outcome to apply. */
  run(): SlashOutcome;
}

/** The commands the compose box understands. Extend this list to add a command;
 *  nothing else needs to change for a new artifact-inserting one, and a new
 *  outcome type (a future `/skills`) is a new `SlashOutcome` variant plus one
 *  branch where the panel applies it. */
export const SLASH_COMMANDS: readonly SlashCommand[] = [
  {
    name: 'jd',
    aliases: ['posting', 'job'],
    hint: 'Show the original job posting',
    run: () => ({ type: 'artifact', artifact: 'job_description' }),
  },
  {
    name: 'resume',
    aliases: ['cv'],
    hint: 'Show the tailored resume',
    run: () => ({ type: 'artifact', artifact: 'resume' }),
  },
  {
    name: 'cover',
    aliases: ['coverletter', 'letter'],
    hint: 'Show the cover letter',
    run: () => ({ type: 'artifact', artifact: 'cover_letter' }),
  },
];

/** True when the draft is a slash invocation (starts with `/` and a name). */
export function isSlashDraft(input: string): boolean {
  return /^\/\S/.test(input.trimStart());
}

/** The bare command word a `/name` draft names, lower-cased, or null when the
 *  draft is not a slash invocation. `/jd` and `/JD  ` both give `jd`. */
function slashWord(input: string): string | null {
  const m = /^\/(\S+)/.exec(input.trim());
  return m ? m[1].toLowerCase() : null;
}

/** Resolve a `/name` draft to its command, matching the name or any alias, or
 *  null when the text is not a slash draft or names no known command. */
export function matchSlashCommand(input: string): SlashCommand | null {
  const word = slashWord(input);
  if (word === null) return null;
  return (
    SLASH_COMMANDS.find(
      (c) => c.name === word || (c.aliases?.includes(word) ?? false),
    ) ?? null
  );
}

/** The commands whose name or an alias starts with the partial word after `/`,
 *  for the autocomplete hint. Empty when the draft is not a slash draft. A bare
 *  `/` lists every command. */
export function slashSuggestions(input: string): SlashCommand[] {
  const trimmed = input.trimStart();
  if (!trimmed.startsWith('/')) return [];
  // Only while still typing the first word (no space yet): once the command is
  // followed by text it is either a match or nothing, not a suggestion list.
  const rest = trimmed.slice(1);
  if (/\s/.test(rest)) return [];
  const partial = rest.toLowerCase();
  return SLASH_COMMANDS.filter(
    (c) =>
      c.name.startsWith(partial) ||
      (c.aliases?.some((a) => a.startsWith(partial)) ?? false),
  );
}

// ── model marker convention ────────────────────────────────────────────────

/** The sentinel the model ends a reply with to attach a saved document, e.g.
 *  `⟦artifact:resume⟧`. The white square brackets are vanishingly unlikely in
 *  ordinary prose, so parsing them out is unambiguous. The capture is anything
 *  up to the closing bracket (not just the valid names) so a malformed marker
 *  like `⟦artifact:⟧` or `⟦artifact:bogus⟧` is still matched and then dropped by
 *  the enum check, never left in the text as a raw sentinel. */
const MARKER_GLOBAL = /⟦artifact:([^⟧]*)⟧/g;

/** A trailing, still-arriving marker mid-stream: an opening `⟦` with the marker
 *  prefix typed so far but not yet closed. Stripped so the raw sentinel never
 *  flashes in the streamed text before its closing bracket lands. */
const MARKER_PARTIAL_TAIL = /⟦(?:a(?:r(?:t(?:i(?:f(?:a(?:c(?:t(?::[a-z_]*)?)?)?)?)?)?)?)?)?$/;

/** One piece of a finalized assistant reply: a run of prose, or an artifact to
 *  render as a card in the reply's flow. */
export type ReplySegment = { text: string } | { artifact: ArtifactKind };

/** Split a finalized assistant reply into ordered prose runs and artifact cards.
 *  A marker naming a known artifact becomes a card segment in place; an unknown
 *  or malformed marker is dropped entirely, never shown as raw sentinel text.
 *  Blank prose runs (e.g. the whitespace a trailing marker sat on) are omitted,
 *  so a reply that is only a marker yields just the card. */
export function parseReply(raw: string): ReplySegment[] {
  const segments: ReplySegment[] = [];
  let cursor = 0;
  for (const match of raw.matchAll(MARKER_GLOBAL)) {
    const start = match.index ?? 0;
    const before = raw.slice(cursor, start);
    if (before.trim().length > 0) segments.push({ text: before.trim() });
    const kind = match[1] as ArtifactKind;
    if (KNOWN_ARTIFACTS.includes(kind)) segments.push({ artifact: kind });
    // Unknown marker: skip it (drop the sentinel, render nothing).
    cursor = start + match[0].length;
  }
  const tail = raw.slice(cursor);
  if (tail.trim().length > 0) segments.push({ text: tail.trim() });
  return segments;
}

/** The reply text with every artifact marker removed, including a half-arrived
 *  trailing marker while its token is still streaming, so the sentinel never
 *  appears in the live bubble. Used for the in-flight `pending` text; the
 *  finalized reply goes through {@link parseReply} instead. */
export function stripMarkers(streaming: string): string {
  return streaming
    .replace(MARKER_GLOBAL, '')
    .replace(MARKER_PARTIAL_TAIL, '')
    .replace(/[ \t]+\n/g, '\n');
}

// ── text projections (copy / download bodies) ──────────────────────────────

/** The minimal shape of the canonical `TailoredResume` a card reads to build
 *  its copyable text. Everything is optional so a partial draft still projects. */
interface CanonicalLike {
  target_title?: string | null;
  summary?: string;
  contact?: { full_name?: string } | null;
  roles?: {
    title?: string;
    company?: string;
    bullets?: { text?: string }[];
  }[];
  skills_section?: { skills?: string[] };
}

/** Project the canonical draft to a plain-text resume for copy and download.
 *  Pulls only stored fields (name, target title, summary, each role's tailored
 *  bullets, the skills line) so the text is the draft verbatim, never anything
 *  the model produced here. */
export function resumeToText(canonical: unknown): string {
  const c = (canonical ?? {}) as CanonicalLike;
  const lines: string[] = [];
  const name = c.contact?.full_name?.trim();
  if (name) lines.push(name);
  if (c.target_title) lines.push(c.target_title);
  if (c.summary?.trim()) {
    lines.push('', 'Summary', c.summary.trim());
  }
  const roles = c.roles ?? [];
  if (roles.length > 0) {
    lines.push('', 'Experience');
    for (const role of roles) {
      const heading = [role.title, role.company].filter(Boolean).join(' — ');
      if (heading) lines.push('', heading);
      for (const bullet of role.bullets ?? []) {
        if (bullet.text?.trim()) lines.push(`  - ${bullet.text.trim()}`);
      }
    }
  }
  const skills = c.skills_section?.skills ?? [];
  if (skills.length > 0) {
    lines.push('', 'Skills', skills.join(', '));
  }
  return lines.join('\n').trim();
}

/** The stored `CoverLetter` payload (`cover_payload.json`) a card reads. */
export interface CoverPayload {
  greeting?: string;
  paragraphs?: string[];
  signoff?: string;
}

/** Project a stored cover-letter payload to plain text for copy and download:
 *  the greeting, the body paragraphs blank-line separated, and the sign-off,
 *  all verbatim from the saved file. */
export function coverToText(payload: CoverPayload): string {
  const blocks: string[] = [];
  if (payload.greeting?.trim()) blocks.push(payload.greeting.trim());
  for (const paragraph of payload.paragraphs ?? []) {
    if (paragraph.trim()) blocks.push(paragraph.trim());
  }
  if (payload.signoff?.trim()) blocks.push(payload.signoff.trim());
  return blocks.join('\n\n');
}
