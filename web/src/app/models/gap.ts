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

export interface GapReport {
  matched: GapMatch[];
  weak: GapMatch[];
  /** JD skills with no dataset backing at all (red / true gaps). */
  unknown: JdSkill[];
}
