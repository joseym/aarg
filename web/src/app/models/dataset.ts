/** The source of truth: the person's recorded experience. Nothing may appear
 *  in any output without tracing back to evidence here. Mirrors
 *  `aarg-domain`'s `ResumeDataset` (`GET/PUT /api/dataset`).
 *
 *  Modelled to the depth the frontend needs; the id newtypes are serde
 *  `transparent` so they are plain strings over the wire. */

import type { Contact } from './variant';
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
  metadata: Record<string, unknown>;
}
