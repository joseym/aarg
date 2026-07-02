/** Provenance check (never-fabricate audit): every output line traced back to
 *  its dataset source, or flagged as unrecorded. Mirrors `aarg-domain`'s
 *  `ProvenanceReport` / `LineProvenance`. */

export type ProvenanceStatus = 'verbatim' | 'grounded' | 'unrecorded';

/** Where a line sits in the output. Serde: `"summary"`,
 *  `{ role_bullet: { role_id, bullet_index } }`, or `{ skill: { index } }`. */
/** Where a checked line sits in the draft. Serde: internally tagged by `kind`,
 *  e.g. `{"kind":"skill","index":0}` / `{"kind":"role_bullet","role_id":"role-1",
 *  "bullet_index":0}` / `{"kind":"summary"}`. */
export type LineLocation =
  | { kind: 'summary' }
  | { kind: 'role_bullet'; role_id: string; bullet_index: number }
  | { kind: 'skill'; index: number };

/** The dataset item a line resolved to. Serde: internally tagged by `type`,
 *  e.g. `{"type":"bullet","id":"bullet-1"}` / `{"type":"skill","id":"skill-83"}`
 *  / `{"type":"summary"}`. */
export type SourceRef =
  | { type: 'summary' }
  | { type: 'bullet'; id: string }
  | { type: 'skill'; id: string };

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
