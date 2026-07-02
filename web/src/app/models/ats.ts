/** Deterministic ATS projection report (a build's `ats_report.json`):
 *  which JD keywords the resume hits, which it misses, and overall coverage.
 *  Mirrors the CLI's `AtsReport`. */

export type KeywordKind = 'hard' | 'soft' | 'domain' | 'phrase' | string;

/** Whether a missed keyword could even be backed by the dataset — the
 *  never-fabricate gate on keyword-coverage suggestions. */
export type EvidenceStatus = 'backed' | 'unbacked' | string;

export interface KeywordHit {
  phrase: string;
  kind: KeywordKind;
}

export interface KeywordMiss {
  phrase: string;
  kind: KeywordKind;
  evidence: EvidenceStatus;
}

export interface AtsReport {
  keyword_hits: KeywordHit[];
  keyword_misses: KeywordMiss[];
  /** 0..1 fraction of JD keywords covered. */
  coverage: number;
}
