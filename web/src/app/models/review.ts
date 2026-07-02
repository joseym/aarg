/** Adversarial review of a canonical draft (FR-3.4). The skeptical reviewer
 *  files structured objections, a score, and notes — it only flags, never
 *  edits. Mirrors `aarg-domain`'s `AdversarialReport` / `Objection`
 *  (a build's `adversarial_report.json`). */

import type { Variant } from './variant';

export type Severity = 'blocking' | 'major' | 'minor';

export type ObjectionKind =
  | 'no_metric'
  | 'vague_verb'
  | 'unsupported_claim'
  | 'generic_phrasing'
  | 'jd_mismatch'
  | 'layout_dense'
  | 'other';

/** Where an objection points. Serde: unit variants are lowercase strings; the
 *  `Bullet(id)` variant is externally tagged as `{ bullet: "<bullet-id>" }`. */
export type ObjectionTarget =
  | 'summary'
  | 'skills_section'
  | 'layout'
  | 'overall'
  | { bullet: string };

/** Whether an objection is about the canonical claim (content) or only a
 *  variant's presentation (layout). Serde: `"canonical"` or
 *  `{ variant_only: "human" | "ats" }`. */
export type ObjectionScope = 'canonical' | { variant_only: Variant };

export interface Objection {
  target: ObjectionTarget;
  severity: Severity;
  kind: ObjectionKind;
  scope: ObjectionScope;
  message: string;
  suggestion: string | null;
}

export interface AdversarialReport {
  objections: Objection[];
  overall_score: number;
  persona_notes: string;
}
