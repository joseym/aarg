/** A past build. `BuildSummary` is the one-line form the sidebar and overview
 *  list bind to (`GET /api/builds`); `BuildDetail` is the full artifact bundle
 *  (`GET /api/builds/:id`). Mirrors the CLI's `history::BuildSummary` and the
 *  serve route's artifact bundle. */

import type { JobRequirements } from './jd';
import type { GapReport } from './gap';
import type { AdversarialReport } from './review';
import type { AtsReport } from './ats';
import type { VariantPayload } from './variant';
import type { CoverBrief, CoverLetter } from './cover';

/** `meta.json`: how a build was produced. */
export interface BuildMeta {
  created_at: string;
  model: string;
  template: string;
  tailor_usage: { input_tokens: number; output_tokens: number };
  subscription: boolean;
}

export interface BuildSummary {
  id: string;
  created_at: string;
  /** `"title @ company"`, or bare title when there's no company. */
  target: string;
  title: string;
  company: string;
  template: string;
  model: string;
  /** Combined score (review + coverage). */
  score: number;
  review_score: number;
  coverage: number;
  objections: number;
  tokens_in: number;
  tokens_out: number;
  /** On a Claude plan — a dollar estimate would mislead. */
  subscription: boolean;
}

/** The canonical draft (`canonical.json`). Modelled loosely here; the tailoring
 *  wave binds its fields directly. */
export interface TailoredResume {
  target_title: string | null;
  contact: { full_name: string; [key: string]: unknown };
  [key: string]: unknown;
}

/** Full bundle from `GET /api/builds/:id` — each artifact present only when the
 *  build has it on disk. */
export interface BuildDetail {
  build_id: string;
  meta?: BuildMeta;
  jd?: JobRequirements;
  gap_report?: GapReport;
  adversarial_report?: AdversarialReport;
  canonical?: TailoredResume;
  /** The rendered human variant payload — the preview shows this and re-renders
   *  it via `POST /api/render`. */
  human_payload?: VariantPayload;
  /** The ATS variant payload (deterministic projection of the canonical draft). */
  ats_payload?: VariantPayload;
  ats_report?: AtsReport;
  /** The drafted cover letter's parsed fields (`cover_payload.json`), not just
   *  its rendered PDF (`GET .../files/cover_letter.pdf`). Absent for a build
   *  that has a tailored résumé but no cover letter drafted yet. */
  cover_payload?: CoverLetter;
  /** The persisted cover-letter interview brief (`cover_brief.json`), when
   *  either surface saved one for this build — the CLI's
   *  `aarg cover --interactive`, the browser's "Draft with copilot", or a
   *  paragraph confirmed as evidence in the Editing view. Feeds the Editing
   *  view's local `checkCoverProvenance` re-check as grounding, the same way
   *  it grounds a fresh draft. Absent for a build with no saved brief. */
  cover_brief?: CoverBrief;
  /** Per-build objection triage (`triage.json`): the objection ids left for now.
   *  Always present (empty when the build has no triage file yet), so the
   *  workspace seeds its "left" set without a missing-key branch. */
  triage?: { left: string[] };
  /** Rendered PDF filenames, fetch each via `GET /api/builds/:id/files/:name`. */
  pdfs: string[];
}
