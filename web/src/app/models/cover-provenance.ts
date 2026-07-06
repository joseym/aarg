/** Cover-letter provenance check: every drafted body paragraph classified by
 *  whether it traces back to the evidence corpus (the résumé, the JD, and any
 *  interview brief), the cover-letter analog of `provenance.ts`'s résumé
 *  check. Mirrors `aarg-domain`'s `cover_provenance` module —
 *  `CoverProvenanceReport` / `ParagraphProvenance` / `CoverParagraphStatus`.
 *  Informational only: nothing here blocks a build or rewrites a letter. */

/** The three-way call `check_cover_provenance` makes on every body paragraph.
 *  `grounded`: every claim traces to the corpus. `unrecorded`: the paragraph
 *  states a number or a specific word the corpus doesn't carry — the one an
 *  editing view surfaces. `exempt`: pure connective framing with no claim to
 *  ground at all ("I'd welcome the chance to discuss this further."). */
export type CoverParagraphStatus = 'grounded' | 'unrecorded' | 'exempt';

/** One classified paragraph. `unbacked_tokens`/`unbacked_digits` are empty
 *  unless `status` is `unrecorded`, in which case they name exactly which
 *  words and numbers the corpus doesn't back. */
export interface ParagraphProvenance {
  text: string;
  status: CoverParagraphStatus;
  unbacked_tokens: string[];
  unbacked_digits: string[];
}

/** A whole letter's provenance, one entry per body paragraph in draft order
 *  — nothing for the greeting or sign-off, which are code-filled and carry no
 *  provenance question. */
export interface CoverProvenanceReport {
  paragraphs: ParagraphProvenance[];
}
