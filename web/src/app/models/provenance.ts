/** Provenance check (never-fabricate audit): every output line traced back to
 *  its dataset source, or flagged as unrecorded. Mirrors `aarg-domain`'s
 *  `ProvenanceReport` / `LineProvenance`. */

export type ProvenanceStatus = 'verbatim' | 'grounded' | 'unrecorded';

/** Where a line sits in the output. Serde: `"summary"`,
 *  `{ role_bullet: { role_id, bullet_index } }`, or `{ skill: { index } }`. */
export type LineLocation =
  | 'summary'
  | { role_bullet: { role_id: string; bullet_index: number } }
  | { skill: { index: number } };

/** The dataset item a line resolved to. Serde: `{ bullet: { id } }`,
 *  `{ skill: { id } }`, or `"summary"`. */
export type SourceRef =
  | { bullet: { id: string } }
  | { skill: { id: string } }
  | 'summary';

export interface SourceMatch {
  source: SourceRef;
  score: number;
}

export interface LineProvenance {
  location: LineLocation;
  text: string;
  status: ProvenanceStatus;
  best_match: SourceMatch | null;
}

export interface ProvenanceReport {
  lines: LineProvenance[];
}
