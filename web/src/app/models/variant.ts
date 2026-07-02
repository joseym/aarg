/** A rendered variant of a build — the Human or ATS projection of the one
 *  canonical draft. Mirrors `aarg-domain`'s `VariantPayload`
 *  (a build's `*-human_payload.json` / ATS payload). Same facts as canonical,
 *  presentation only. */

export type Variant = 'human' | 'ats';

export interface ContactLink {
  label: string;
  url: string;
}

export interface Contact {
  full_name: string;
  email: string;
  phone: string | null;
  location: string | null;
  links: ContactLink[];
}

export interface PayloadBullet {
  /** The canonical bullet this projects (provenance back to the draft). */
  source_id: string;
  text: string;
}

export interface PayloadRole {
  id: string;
  company: string;
  title: string;
  start: string;
  end: string | null;
  location: string | null;
  bullets: PayloadBullet[];
}

export interface PayloadEducation {
  institution: string;
  credential: string | null;
  field: string | null;
  start: string | null;
  end: string | null;
}

export interface SkillsSection {
  skills: string[];
}

export interface SkillGroup {
  label: string;
  skills: string[];
}

export interface LayoutHints {
  sidebar: boolean;
  accent_color: string;
  density: string;
  show_summary: boolean;
  max_pages: number;
}

export interface VariantPayload {
  variant: Variant;
  template: string;
  contact: Contact;
  target_title: string;
  summary: string;
  roles: PayloadRole[];
  education: PayloadEducation[];
  skills_section: SkillsSection;
  skill_groups: SkillGroup[];
  projects: unknown[];
  achievements: unknown[];
  certifications: unknown[];
  layout_hints: LayoutHints;
}
