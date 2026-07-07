/** Cover-letter provenance check: every drafted body paragraph classified by
 *  whether the claims it makes trace back to the candidate's evidence (the
 *  résumé, the JD, and any interview brief), the cover-letter analog of
 *  `provenance.ts`'s résumé check. Mirrors `aarg-domain`'s `cover_provenance`
 *  module — `CoverProvenanceReport` / `ParagraphProvenance` /
 *  `CoverParagraphStatus`. The claim judgment is model-graded (it judges by
 *  meaning, so "payments" grounds against a résumé's "billing"); the number
 *  check stays deterministic. Informational only: nothing here blocks a build
 *  or rewrites a letter. */

/** The three-way call `check_cover_provenance` makes on every body paragraph.
 *  `grounded`: the paragraph's claims trace to the evidence (possibly in
 *  different words) and it states no unbacked number. `unrecorded`: it asserts
 *  experience the evidence doesn't support, or states a number the evidence
 *  doesn't carry — the one an editing view surfaces. `exempt`: pure connective
 *  framing with no claim to ground at all ("I'd welcome the chance to discuss
 *  this further."). */
export type CoverParagraphStatus = 'grounded' | 'unrecorded' | 'exempt';

/** One classified paragraph. `unbacked_claim` is the model's plain-language
 *  account of the unsupported claim (present only when `unrecorded` on content
 *  grounds); `unbacked_digits` names any numbers the evidence doesn't carry
 *  (the deterministic check). Both are absent/empty for a grounded or exempt
 *  paragraph. */
export interface ParagraphProvenance {
  text: string;
  status: CoverParagraphStatus;
  unbacked_claim: string | null;
  unbacked_digits: string[];
}

/** A whole letter's provenance, one entry per body paragraph in draft order
 *  — nothing for the greeting or sign-off, which are code-filled and carry no
 *  provenance question. */
export interface CoverProvenanceReport {
  paragraphs: ParagraphProvenance[];
}
