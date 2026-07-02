/** Gap analysis (FR-1.5): which JD skills the dataset can back, which are
 *  weakly covered, and which are unknown. Mirrors `aarg-domain`'s `GapReport`
 *  (a build's `gap_report.json`). */

import type { JdSkill } from './jd';

/** One matched JD skill, tied to the dataset skill that backs it.
 *  `semantic` is false for an exact/alias match (green) and true for a
 *  model-judged semantic match (amber). */
export interface GapMatch {
  jd_skill: JdSkill;
  skill_id: string;
  dataset_name: string;
  semantic: boolean;
}

/** The importance-weighted coverage the overview screen shows as its headline,
 *  returned by the `weighted_coverage` wasm export (critical=3/required=2/
 *  preferred=1; `score` is 0..1). */
export interface WeightedCoverage {
  score: number;
  matched: number;
  total: number;
  by_importance: Record<string, { matched: number; total: number }>;
}

export interface GapReport {
  matched: GapMatch[];
  weak: GapMatch[];
  /** JD skills with no dataset backing at all (red / true gaps). */
  unknown: JdSkill[];
}
