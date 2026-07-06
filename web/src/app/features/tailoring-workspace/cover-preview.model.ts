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
 *  paragraph it names the exact words and numbers the corpus does not carry,
 *  so the reader can see precisely what to confirm or reword. */
export function coverStatusExplainer(p: ParagraphProvenance): string {
  switch (p.status) {
    case 'grounded':
      return "This paragraph's facts all trace to your resume, the posting, or what you told the interview.";
    case 'exempt':
      return 'This is connecting language with no specific claim to check.';
    case 'unrecorded': {
      const items = [...p.unbacked_tokens, ...p.unbacked_digits];
      const base =
        'This paragraph mentions something not found in your resume, the posting, or your interview answers';
      return items.length === 0 ? `${base}.` : `${base}: ${items.join(', ')}.`;
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
