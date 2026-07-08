/** Pure display helpers for the cover-letter editing view. These turn a
 *  `ParagraphProvenance` (the wasm classifier's per-paragraph verdict) into the
 *  short label, popover explanation, and informational badge text the view
 *  renders. Kept separate from the component so the wording is unit-testable
 *  without standing up Angular. */

import type { CoverParagraphStatus, ParagraphProvenance } from '../../models';

/** A short human label for a paragraph's provenance status, adapted for prose.
 *  `exempt` reads as neutral connective language, never as a flag and never as
 *  a gold star — there is simply nothing in it to trace. */
export function coverStatusLabel(status: CoverParagraphStatus): string {
  switch (status) {
    case 'grounded':
      return 'Traced to your evidence';
    case 'unrecorded':
      return 'Needs a look';
    case 'exempt':
      return 'Connecting language';
  }
}

/** The popover explanation for a paragraph's status. For an `unrecorded`
 *  paragraph it leads with the model's plain-language account of the
 *  unsupported claim (when there is one) and names any unbacked numbers, so the
 *  reader can see precisely what to confirm or reword. */
export function coverStatusExplainer(p: ParagraphProvenance): string {
  switch (p.status) {
    case 'grounded':
      return "This paragraph's facts all trace to your resume or the posting.";
    case 'exempt':
      return 'This is connecting language with no specific claim to check.';
    case 'unrecorded': {
      const parts: string[] = [];
      if (p.unbacked_claim?.trim()) parts.push(p.unbacked_claim.trim());
      if (p.unbacked_digits.length > 0) {
        const label = p.unbacked_digits.length === 1 ? 'a number' : 'numbers';
        parts.push(`states ${label} your evidence doesn't carry: ${p.unbacked_digits.join(', ')}`);
      }
      const base = 'This paragraph mentions something not found in your resume or the posting';
      return parts.length === 0 ? `${base}.` : `This paragraph ${parts.join('; and ')}.`;
    }
  }
}

/** The informational claim badge's text: how many body paragraphs still read as
 *  unrecorded, out of the total classified. Plain and honest — it counts, it
 *  never gates. */
export function coverBadgeText(unrecorded: number, total: number): string {
  if (total === 0) return 'No paragraphs to check yet';
  if (unrecorded === 0) return 'Every paragraph traces to your evidence';
  const paras = total === 1 ? 'paragraph' : 'paragraphs';
  const verb = unrecorded === 1 ? 'needs' : 'need';
  return `${unrecorded} of ${total} ${paras} ${verb} a look`;
}

/** The inline warning shown next to the Save button while unrecorded paragraphs
 *  are present, so a person saving them can't miss that they aren't yet traced
 *  to their evidence. Returns null when nothing is unrecorded (no warning to
 *  show). Saving is never blocked by this — it only informs; the server's digit
 *  guard is the sole hard gate. */
export function coverUnrecordedFlag(unrecorded: number): string | null {
  if (unrecorded <= 0) return null;
  const paras = unrecorded === 1 ? 'paragraph' : 'paragraphs';
  const verb = unrecorded === 1 ? "isn't" : "aren't";
  return `${unrecorded} ${paras} ${verb} traced to your evidence yet. Saving keeps them as written.`;
}

/** The message shown after a save resolves. Three cases, most severe first:
 *  the server dropped one or more paragraphs (a hand-edited figure the evidence
 *  doesn't back — the hard gate, reported distinctly from the informational
 *  flag below); the save landed but unrecorded paragraphs remain (informational,
 *  never blocked); or a clean save. `dropped` and `unrecorded` are counts read
 *  after the save, so the message reflects what actually persisted. */
export function coverSaveMessage(dropped: number, unrecorded: number): string {
  if (dropped > 0) {
    const paras = dropped === 1 ? 'paragraph' : 'paragraphs';
    const verb = dropped === 1 ? 'was' : 'were';
    return `Saved. ${dropped} ${paras} with an unverified number ${verb} dropped.`;
  }
  if (unrecorded > 0) {
    const paras = unrecorded === 1 ? 'paragraph' : 'paragraphs';
    const verb = unrecorded === 1 ? "isn't" : "aren't";
    return `Saved, but ${unrecorded} ${paras} still ${verb} traced to your evidence.`;
  }
  return 'Saved your cover letter edits.';
}
