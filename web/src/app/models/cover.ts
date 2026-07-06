/** The cover-letter types mirror `aarg`'s `CoverLetter` and the serve route's
 *  `POST /api/builds/:id/cover` response. The letter is drafted from the build's
 *  canonical résumé and JD; its `contact` block and `signoff` come from the
 *  résumé, filled by code inside the agent, so the same never-fabricate posture
 *  holds. */

import type { Contact } from './variant';
import type { TokenUsage } from './llm';

/** A finished cover letter: a greeting and sign-off wrapped around the model's
 *  body paragraphs, with the recipient and contact block drawn from the résumé
 *  and JD. */
export interface CoverLetter {
  contact: Contact;
  company: string;
  title: string;
  greeting: string;
  paragraphs: string[];
  signoff: string;
}

/** The `POST /api/builds/:id/cover` success body. */
export interface GenerateCoverResponse {
  letter: CoverLetter;
  warnings: string[];
  /** The persisted PDF filename (`cover_letter.pdf`) the preview fetches. */
  pdf: string;
  model: string;
  usage: TokenUsage;
}

/** What a cover-letter interview (`cover_interview_interactive`) recorded:
 *  the letter's angle, what to emphasize, its tone, why this role and
 *  company, and any constraints — every field optional or empty, so a
 *  partial or skipped interview still yields a usable brief. Mirrors
 *  `aarg-domain`'s `CoverBrief` exactly; passed straight through to
 *  `POST /api/builds/:id/cover` as grounding, never edited in the browser. */
export interface CoverBrief {
  angle?: string | null;
  emphasis: string[];
  tone?: string | null;
  motivation?: string | null;
  constraints: string[];
}
