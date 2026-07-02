/** Parsed job posting. Mirrors `aarg-domain`'s `JobRequirements`
 *  (see a build's `jd.json` artifact). */

export type SkillImportance = 'critical' | 'required' | 'preferred' | 'optional';
export type SkillCategory = 'hard' | 'soft' | 'domain' | 'tool' | string;

/** A single skill the posting asks for, with the JD phrases that motivated it. */
export interface JdSkill {
  name: string;
  category: SkillCategory;
  importance: SkillImportance;
  context_phrases: string[];
}

export interface JobRequirements {
  company: string;
  title: string;
  seniority: string | null;
  location: string | null;
  remote: string | null;
  domain_keywords: string[];
  required_skills: JdSkill[];
  preferred_skills: JdSkill[];
  responsibilities: string[];
  ats_phrases: string[];
  raw_text: string;
  source_url: string | null;
}
