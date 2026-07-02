/** The source of truth: the person's recorded experience. Nothing may appear
 *  in any output without tracing back to evidence here. Mirrors
 *  `aarg-domain`'s `ResumeDataset` (`GET/PUT /api/dataset`).
 *
 *  Modelled to the depth the frontend needs; the id newtypes are serde
 *  `transparent` so they are plain strings over the wire. */

import type { Contact } from './variant';
import type { ObjectionKind } from './review';

/** An objection the user has accepted as intentional, so the reviewer stops
 *  re-flagging it. Keyed by `(target, kind)` — the same signature the backend
 *  dismissal uses. The objection-triage "Accept as intentional" action appends
 *  one of these and `PUT`s the dataset.
 *
 *  `target` is the DOMAIN string Rust's `DismissedObjection.target: String`
 *  serialises — `"bullet:<id>"`, `"summary"`, `"skills"`, `"layout"`, `"overall"`
 *  — NOT the wire `ObjectionTarget` (which is `{bullet}` / `"skills_section"`).
 *  Use `targetKey()` to derive it from an objection's wire target. */
export interface DismissedObjection {
  target: string;
  kind: ObjectionKind;
}

/** `dataset.metadata`. Typed for the fields the UI reads/writes; the index
 *  signature keeps forward-compatibility with fields the frontend ignores. */
export interface DatasetMetadata {
  created_at?: string;
  updated_at?: string;
  source_files?: string[];
  declined_skills?: string[];
  dismissed_objections?: DismissedObjection[];
  [key: string]: unknown;
}
import type { SkillCategory } from './jd';

export type Proficiency = 'expert' | 'advanced' | 'proficient' | 'familiar' | string;
export type Strength = 'strong' | 'standard' | 'weak' | string;

/** A dataset value in `YYYY-MM` form (serde serialises `YearMonth` as a string). */
export type YearMonth = string;

export interface EvidenceRef {
  [key: string]: unknown;
}

export interface Skill {
  id: string;
  canonical_name: string;
  aliases: string[];
  category: SkillCategory;
  proficiency: Proficiency;
  years: number | null;
  last_used: YearMonth | null;
  /** A skill with no evidence fails validation and is excluded from tailoring. */
  evidence: EvidenceRef[];
  verified: boolean;
  verified_at: string | null;
}

export interface SkillGraph {
  skills: Skill[];
  aliases: Record<string, string>;
}

export interface Bullet {
  id: string;
  text: string;
  skill_ids: string[];
  metric: unknown | null;
  theme: unknown[];
  strength: Strength;
  variants: string[];
}

export interface Role {
  id: string;
  company: string;
  title: string;
  start: YearMonth;
  end: YearMonth | null;
  location: string | null;
  employment_type: string;
  bullets: Bullet[];
  skill_ids: string[];
  context: string | null;
}

export interface ResumeDataset {
  schema_version: number;
  contact: Contact;
  summary: string | null;
  summary_confirmed: boolean;
  roles: Role[];
  education: unknown[];
  skills: SkillGraph;
  projects: unknown[];
  certifications: unknown[];
  achievements: unknown[];
  publications: unknown[];
  languages: unknown[];
  voice_samples: unknown[];
  metadata: DatasetMetadata;
}
