//! Deterministic per-paragraph provenance for a drafted cover letter:
//! classify each body paragraph by whether it traces back to the
//! candidate's own recorded evidence, so an editing view can show where
//! every paragraph came from. The résumé has the same idea in
//! [`provenance`](crate::provenance) — this is the cover-letter analog,
//! with a shape purpose-built for prose.
//!
//! A cover paragraph is not a reworded copy of one recorded line the way a
//! résumé bullet is: it paraphrases many facts at once and stitches them
//! together with ordinary connective language ("I'd welcome the chance
//! to..."). So the résumé module's single-best-source match doesn't carry
//! over. Instead a paragraph is checked against the evidence corpus as a
//! whole. Two things are checked, each against its own corpus because the
//! two draw the line at a different place:
//! - every claim-bearing word the paragraph states must appear in the word
//!   bag built from the candidate's tailored résumé, the job posting, and
//!   the cover-letter [`CoverBrief`](crate::cover_interview::CoverBrief) if
//!   one was gathered. The posting belongs here: echoing a role or company
//!   descriptor ("platform reliability at scale") or a required skill name
//!   is legitimate, since it describes the role, not a personal claim.
//! - every number the paragraph states must be a number the *résumé or
//!   brief* states — the posting's numbers are excluded. A number in the
//!   posting states what the role requires ("5+ years"), never what the
//!   candidate has done, so it must never ground a personal-history claim.
//!   This digit set is [`cover`](crate::cover)'s shipped `allowed_digits`
//!   guard reused verbatim, so the two can never drift.
//!
//! Each paragraph lands in one of three buckets
//! ([`CoverParagraphStatus`]):
//! - **grounded** — every number and every claim-bearing word traces to
//!   the corpus. A paragraph that paraphrases the résumé, echoes the
//!   posting's language, or builds on something the candidate said in the
//!   interview brief lands here.
//! - **unrecorded** — the paragraph introduces a number, or a specific
//!   claim-bearing word (a skill, an employer, a technology, a scope
//!   term), that is nowhere in the corpus. This is the one an editing view
//!   would surface: it reads as content the candidate never recorded.
//! - **exempt** — once ordinary connective and framing language is set
//!   aside, the paragraph makes no specific claim at all: pure first-person
//!   framing like "I'd welcome the opportunity to discuss this further."
//!   Not a flag, and not "grounded" either, because there is nothing to
//!   ground. This third state is the piece the résumé module has no analog
//!   for — a résumé line is always *some* recorded fact, so it is either
//!   traceable or it isn't; a cover letter genuinely contains connective
//!   sentences that assert nothing, and calling those "unrecorded" would
//!   flag benign prose as if it were fabricated.
//!
//! **Informational, not enforcement.** Nothing here blocks a build or
//! rewrites a letter. The structural never-fabricate guard for cover
//! letters lives in [`cover::assemble`](crate::cover): a paragraph that
//! states a number the résumé and brief don't back is dropped there,
//! before it can reach a rendered letter. This module runs on top of what
//! already passed that gate — for an editing view, or later for a
//! candidate's own hand-edit — and reports; it never rejects. An
//! `unrecorded` paragraph is not a violation: never-fabricate governs what
//! the *model* may claim, never what the *candidate* may choose to write.
//!
//! Two deliberate scope decisions:
//! - The greeting and sign-off are never classified. They are filled by
//!   code from the posting's company and the résumé's contact block (see
//!   [`cover`](crate::cover)), never authored by the model, so they carry
//!   no provenance question — and they are not part of
//!   [`CoverLetter::paragraphs`](crate::cover::CoverLetter), so the loop
//!   below never sees them.
//! - The candidate's `voice_samples` are excluded from the corpus. They
//!   anchor tone during generation ("match this voice, do not reuse its
//!   content"), so treating them as evidence would license letter content
//!   to leak in from unrelated writing. They are not a parameter of
//!   [`check_cover_provenance`] at all, so a term that appears only in a
//!   voice sample can never ground a paragraph.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::cover::{CoverLetter, allowed_digits};
use crate::cover_interview::CoverBrief;
use crate::jd::JobRequirements;
use crate::keywords::keyword_key;
use crate::tailor::{TailoredResume, digit_runs};

/// A cover-letter interview brief caps how many emphasis and constraint
/// items it shows the model (see [`cover`](crate::cover)'s `allowed_digits`
/// and the generation prompt). The corpus mirrors that cap, so a
/// hand-edited or reused brief with a long list can't quietly widen what
/// counts as grounded past what the letter could actually have drawn on.
const BRIEF_LIST_CAP: usize = 8;

/// The three-way call [`check_cover_provenance`] makes on every paragraph.
/// See the module doc for what each one means, and — as important — what it
/// does not: this is not the never-fabricate gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverParagraphStatus {
    Grounded,
    Unrecorded,
    Exempt,
}

/// One classified paragraph: its text, the call, and — when the call is
/// `unrecorded` — exactly which words and numbers weren't found in the
/// corpus, so an editing view can point at them. Both lists are empty for
/// a `grounded` or `exempt` paragraph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParagraphProvenance {
    pub text: String,
    pub status: CoverParagraphStatus,
    /// Claim-bearing words in the paragraph that the corpus does not
    /// contain (sorted, deduped). Numbers are reported separately in
    /// `unbacked_digits`, not here.
    pub unbacked_tokens: Vec<String>,
    /// Numbers the paragraph states that the corpus does not (sorted,
    /// deduped) — a percentage, a count, a team size the evidence never
    /// mentions.
    pub unbacked_digits: Vec<String>,
}

/// A whole letter's provenance, one entry per body paragraph in draft
/// order — nothing for the greeting or sign-off, which are code-filled and
/// carry no provenance question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverProvenanceReport {
    pub paragraphs: Vec<ParagraphProvenance>,
}

/// Classify every body paragraph of `letter` against the evidence corpus
/// built from the tailored `resume`, the `jd`, and the interview `brief`
/// when one was gathered. See the module doc for the three statuses and
/// the (deliberate) absence of any hard failure here — this never returns
/// an `Err`, because it reports rather than gates.
///
/// The word bag is the résumé plus the posting plus the brief, the same
/// material the generation step draws on; the number set is the résumé and
/// brief only, because a posting's numbers describe the role, not the
/// candidate (see [`token_corpus_texts`] and the module doc). Neither
/// corpus includes the candidate's voice samples — they are not even a
/// parameter here.
pub fn check_cover_provenance(
    letter: &CoverLetter,
    resume: &TailoredResume,
    jd: &JobRequirements,
    brief: Option<&CoverBrief>,
) -> CoverProvenanceReport {
    // Tokens may come from the whole corpus — the résumé, the posting, and
    // the brief — because echoing the posting's language (a skill name, a
    // role or company descriptor) is legitimate. Digits may not: a number
    // in the posting states what the *role* requires ("5+ years"), never
    // what the *candidate* has done, so the digit set is the résumé and
    // brief only, taken straight from `cover.rs`'s shipped guard.
    let corpus_tokens: HashSet<String> = token_corpus_texts(resume, jd, brief)
        .iter()
        .flat_map(|t| keyword_key(t))
        .collect();
    let corpus_digits: HashSet<String> = allowed_digits(resume, brief);

    let paragraphs = letter
        .paragraphs
        .iter()
        .map(|paragraph| classify_paragraph(paragraph, &corpus_tokens, &corpus_digits))
        .collect();

    CoverProvenanceReport { paragraphs }
}

/// Every stretch of text the *token* corpus is built from, gathered once.
/// This is the résumé (summary, target title, each role's company and title
/// and bullet text, and the skills), the posting (title, company, required
/// and preferred skill names, responsibilities), and the interview brief
/// (angle, emphasis, tone, motivation, constraints) when one is present.
///
/// The posting belongs in the *word* bag: a cover letter legitimately
/// echoes the posting's own language — a required skill name, a phrase
/// describing the role or company ("platform reliability at scale") — and
/// flagging that as unrecorded would be the common false alarm. Those
/// tokens describe the role, not a claim about what the candidate has
/// personally done.
///
/// The posting deliberately does **not** feed the *digit* corpus, which is
/// built separately by [`cover`](crate::cover)'s `allowed_digits` from the
/// résumé and brief only. A number in the posting states what the role
/// *requires* ("5+ years of platform experience", "a team of 10+"), never
/// what the candidate has done. If a paragraph says "I have 5 years of
/// experience with X" and the only "5" in sight is the posting's stated
/// requirement, calling that grounded would vouch for a personal-history
/// claim nothing the candidate recorded actually backs. So JD requirement
/// numbers can never be evidence of candidate experience, regardless of who
/// calls this — a freshly generated letter that already passed
/// `assemble`'s digit gate or a candidate's hand-edited paragraph that
/// never did.
fn token_corpus_texts(
    resume: &TailoredResume,
    jd: &JobRequirements,
    brief: Option<&CoverBrief>,
) -> Vec<String> {
    let mut texts: Vec<String> = Vec::new();

    // Résumé.
    texts.push(resume.summary.clone());
    if let Some(title) = &resume.target_title {
        texts.push(title.clone());
    }
    for role in &resume.roles {
        texts.push(role.company.clone());
        texts.push(role.title.clone());
        for bullet in &role.bullets {
            texts.push(bullet.text.clone());
        }
    }
    for skill in &resume.skills_section.skills {
        texts.push(skill.clone());
    }

    // Posting.
    texts.push(jd.title.clone());
    texts.push(jd.company.clone());
    texts.extend(jd.required_skills.iter().map(|s| s.name.clone()));
    texts.extend(jd.preferred_skills.iter().map(|s| s.name.clone()));
    texts.extend(jd.responsibilities.iter().cloned());

    // Interview brief.
    if let Some(brief) = brief {
        if let Some(angle) = &brief.angle {
            texts.push(angle.clone());
        }
        texts.extend(brief.emphasis.iter().take(BRIEF_LIST_CAP).cloned());
        if let Some(tone) = &brief.tone {
            texts.push(tone.clone());
        }
        if let Some(motivation) = &brief.motivation {
            texts.push(motivation.clone());
        }
        texts.extend(brief.constraints.iter().take(BRIEF_LIST_CAP).cloned());
    }

    texts
}

/// Classify one paragraph against the prepared corpus sets.
///
/// The order of the checks is the whole design. First, anything specific
/// the paragraph states that the corpus doesn't back — an unknown number
/// or an unknown claim-bearing word — makes it `unrecorded`. Only if
/// nothing is unbacked do we ask the second question: did the paragraph
/// make any specific claim at all? A paragraph that is pure connective
/// framing has no claim-bearing words left once filler is stripped, so it
/// is `exempt`; one that does state backed claims is `grounded`.
fn classify_paragraph(
    paragraph: &str,
    corpus_tokens: &HashSet<String>,
    corpus_digits: &HashSet<String>,
) -> ParagraphProvenance {
    let tokens = claim_tokens(paragraph);
    let unbacked_tokens: Vec<String> = tokens
        .iter()
        .filter(|t| !corpus_tokens.contains(*t))
        .cloned()
        .collect();

    let mut unbacked_digits: Vec<String> = digit_runs(paragraph)
        .into_iter()
        .filter(|d| !corpus_digits.contains(d))
        .collect();
    unbacked_digits.sort();

    let status = if !unbacked_tokens.is_empty() || !unbacked_digits.is_empty() {
        CoverParagraphStatus::Unrecorded
    } else if tokens.is_empty() {
        CoverParagraphStatus::Exempt
    } else {
        CoverParagraphStatus::Grounded
    };

    ParagraphProvenance {
        text: paragraph.to_string(),
        status,
        unbacked_tokens,
        unbacked_digits,
    }
}

/// The claim-bearing words of a paragraph: [`keyword_key`]'s normalized
/// tokens, minus ordinary prose filler, minus bare numbers.
///
/// [`keyword_key`] was built to compare *keywords* — it lowercases, stems,
/// dedupes, and drops a small set of résumé noise words (seniority, "with",
/// "the"). It does not drop the connective and framing language a full
/// sentence is mostly made of ("I would welcome the chance to discuss..."),
/// so on its own it would leave a purely connective paragraph looking full
/// of unrecorded words. [`PROSE_FILLER`] removes that layer, so what
/// remains is the specific content: skills, employers, technologies, scope
/// terms, domain words. Bare numbers are dropped here too — they are
/// checked against the corpus's number set separately, so a stray figure
/// is reported once, as a digit, not twice.
///
/// The list is deliberately generic. It holds only closed-class function
/// words and high-frequency framing words that are never themselves a
/// résumé claim; it holds no skill, employer, technology, or domain term.
/// That is what keeps it from hiding a real gap: a fabricated skill or
/// company is a specific word, never a generic one, so it survives the
/// filter and gets flagged. The cost is the other direction — a paragraph
/// that rephrases a recorded fact with a fresh strong verb the corpus
/// doesn't carry ("spearheaded" for a recorded "led") may read as
/// `unrecorded`. For an informational view that is the safe way to be
/// wrong: it points at real rewording for the candidate to confirm rather
/// than quietly vouching for a claim the evidence doesn't literally carry.
fn claim_tokens(text: &str) -> Vec<String> {
    let filler = filler_stems();
    keyword_key(text)
        .into_iter()
        .filter(|t| !filler.contains(t))
        .filter(|t| !t.bytes().all(|b| b.is_ascii_digit()))
        .collect()
}

/// [`PROSE_FILLER`] run through [`keyword_key`], so the filler words are
/// stemmed and normalized the exact same way a paragraph's words are —
/// otherwise "discuss" in the filler list would never match a stemmed
/// "discuss" from a paragraph. Any filler word that is itself résumé noise
/// (like "the" or "experience") reduces to nothing here, which is
/// harmless: the paragraph side already dropped it too.
fn filler_stems() -> HashSet<String> {
    PROSE_FILLER.iter().flat_map(|w| keyword_key(w)).collect()
}

/// Ordinary English filler and cover-letter framing language — the words a
/// paragraph is stitched together with, as opposed to the specific claims
/// it makes. Removing these is what lets a purely connective paragraph
/// come out `exempt`, and a rephrased-but-recorded one come out `grounded`,
/// instead of looking like a wall of unrecorded words.
///
/// **Why this is a broad list, not a short hand-picked one.** An earlier
/// version held a dozen or so words tuned against a handful of synthetic
/// test sentences. On real senior-level prose it failed badly: every
/// paragraph of a genuine generated letter came back `unrecorded`, flagged
/// on ordinary connective and evaluative vocabulary ("rigor", "clarity",
/// "traceable", "auditable", "crisis", "partnership", even the bare
/// function word "when"). None of those are fabricated facts; the letter
/// had simply rephrased recorded facts in words the résumé's own bullets
/// don't use. A manually-curated denylist can't keep up with natural
/// language — there are too many ways to say the same non-claim — so the
/// classifier drowned its real signal in false alarms and trained the
/// reader to ignore every flag. The fix is to stop hand-picking and instead
/// build the filler from two principled tiers.
///
/// **Tier 1 — a standard English stopword base.** Closed-class function
/// words: articles, determiners, quantifiers, prepositions, conjunctions,
/// pronouns and possessives, auxiliary and copula verb forms, and the
/// common adverbs — the classic SMART/NLTK-style list. These are
/// structurally incapable of being a specific claim, so no judgment call is
/// needed for this tier; the point is only to be *complete* (the old list's
/// gaps here are why a bare "when" survived as a supposed claim word).
///
/// **Tier 2 — a curated professional/evaluative supplement.** Generic
/// words that describe the *manner*, *quality*, or *approach* of work
/// rather than a concrete, checkable fact — plus the cover-letter framing
/// vocabulary a letter is scaffolded with (politeness, sentiment, the
/// generic nouns and verbs of applying for a job). "rigor", "clarity",
/// "foundational", "systematically", "meaningfully", "partnership" belong
/// here; a specific skill, employer, technology, metric, or domain term
/// never does.
///
/// That tier-2 boundary is load-bearing and is the module's stated
/// invariant: a fabricated fact is always a *specific* word (a skill name,
/// an employer, a technology, a number, a domain term), so it is never on
/// this list and never filtered away — only generic connective and
/// evaluative tissue is. In particular the payments/billing family is
/// deliberately absent: "pay"/"payments" is a genuine business-domain term,
/// so an unbacked claim about handling payments must stay flagged. (It also
/// needs no special-casing here: [`keyword_key`] already stems "payments",
/// "payment", and "pay" to the same root, so when the résumé or posting
/// really does name payments, a paragraph echoing it grounds through the
/// corpus on its own.)
const PROSE_FILLER: &[&str] = &[
    // === Tier 1: standard English stopwords (function words) ===
    //
    // Pronouns, possessives, and reflexives.
    "i",
    "me",
    "my",
    "mine",
    "myself",
    "we",
    "us",
    "our",
    "ours",
    "ourselves",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
    "they",
    "them",
    "their",
    "theirs",
    "themselves",
    "it",
    "its",
    "itself",
    "he",
    "him",
    "his",
    "himself",
    "she",
    "her",
    "hers",
    "herself",
    "one",
    "ones",
    "oneself",
    // Interrogatives and relatives.
    "who",
    "whom",
    "whose",
    "which",
    "what",
    "when",
    "where",
    "why",
    "how",
    "whatever",
    "whenever",
    "wherever",
    "whoever",
    "whichever",
    "that",
    "this",
    "these",
    "those",
    "there",
    "here",
    // Contraction fragments left behind after splitting on the apostrophe
    // (I'd, I'll, I've, I'm, you're, it's, don't, can't).
    "d",
    "ll",
    "ve",
    "m",
    "re",
    "s",
    "t",
    "o",
    "y",
    // Articles, determiners, quantifiers.
    "the",
    "a",
    "an",
    "some",
    "any",
    "all",
    "each",
    "every",
    "both",
    "either",
    "neither",
    "no",
    "not",
    "nothing",
    "none",
    "many",
    "much",
    "few",
    "fewer",
    "several",
    "most",
    "more",
    "less",
    "least",
    "other",
    "another",
    "such",
    "same",
    "own",
    "enough",
    // Prepositions and conjunctions.
    "of",
    "in",
    "on",
    "at",
    "to",
    "for",
    "from",
    "by",
    "with",
    "without",
    "within",
    "into",
    "onto",
    "upon",
    "about",
    "as",
    "than",
    "then",
    "so",
    "and",
    "or",
    "but",
    "nor",
    "yet",
    "if",
    "because",
    "since",
    "while",
    "whereas",
    "though",
    "although",
    "unless",
    "until",
    "whether",
    "over",
    "under",
    "above",
    "below",
    "across",
    "through",
    "throughout",
    "between",
    "among",
    "amongst",
    "during",
    "before",
    "after",
    "toward",
    "towards",
    "per",
    "via",
    "out",
    "up",
    "off",
    "down",
    "near",
    "along",
    "around",
    "against",
    "beyond",
    "behind",
    "once",
    "onwards",
    // Auxiliaries, modals, and the most generic verbs of being and doing.
    "be",
    "am",
    "is",
    "are",
    "was",
    "were",
    "been",
    "being",
    "have",
    "has",
    "had",
    "having",
    "do",
    "does",
    "did",
    "doing",
    "will",
    "would",
    "shall",
    "should",
    "can",
    "could",
    "may",
    "might",
    "must",
    "ought",
    "need",
    "get",
    "got",
    "getting",
    "make",
    "makes",
    "made",
    "making",
    "take",
    "takes",
    "took",
    "taken",
    "taking",
    "put",
    "come",
    "came",
    "go",
    "goes",
    "went",
    "going",
    "give",
    "gives",
    "gave",
    "given",
    "build",
    "builds",
    "built",
    "building",
    "time",
    "times",
    "spend",
    "spends",
    "spent",
    "suit",
    "suits",
    "suited",
    // Adverbs and intensifiers.
    "very",
    "really",
    "truly",
    "quite",
    "rather",
    "just",
    "only",
    "also",
    "too",
    "well",
    "even",
    "still",
    "again",
    "ever",
    "never",
    "always",
    "often",
    "sometimes",
    "usually",
    "currently",
    "recently",
    "previously",
    "now",
    "today",
    "especially",
    "particularly",
    "additionally",
    "furthermore",
    "moreover",
    "however",
    "therefore",
    "thus",
    "hence",
    "indeed",
    "certainly",
    "simply",
    "personally",
    "genuinely",
    "greatly",
    "highly",
    "strongly",
    "deeply",
    "closely",
    "directly",
    "exactly",
    "meaningfully",
    "systematically",
    // === Tier 2: curated professional / evaluative / framing supplement ===
    //
    // Generic manner, quality, and approach words — they describe *how*
    // work was done, never a specific, checkable fact. A fabricated skill,
    // employer, technology, metric, or domain term is always a more
    // specific word than any of these, so it survives the filter and is
    // still flagged.
    "ability",
    "able",
    "alone",
    "challenge",
    "challenging",
    "clarity",
    "clear",
    "compromise",
    "deliver",
    "delivering",
    "delivered",
    "delivery",
    "foundational",
    "foundation",
    "rigor",
    "rigorous",
    "traceable",
    "auditable",
    "crisis",
    "retrofit",
    "peripheral",
    "multifaceted",
    "critical",
    "parallel",
    "partnership",
    "meaningful",
    "last",
    "force",
    "ignore",
    "lay",
    "start",
    "run",
    "ran",
    "grew",
    "grow",
    "growing",
    "manner",
    "approach",
    "thorough",
    "careful",
    "deliberate",
    "intentional",
    "practical",
    "pragmatic",
    "disciplined",
    "discipline",
    // Cover-letter framing: politeness, sentiment, and the generic nouns
    // and verbs a letter is scaffolded with. None of these is a specific
    // claim about the candidate's experience.
    "welcome",
    "opportunity",
    "chance",
    "discuss",
    "discussion",
    "conversation",
    "chat",
    "talk",
    "speak",
    "hope",
    "forward",
    "hear",
    "reach",
    "glad",
    "happy",
    "pleased",
    "delighted",
    "eager",
    "keen",
    "excited",
    "enthusiastic",
    "interested",
    "interest",
    "thank",
    "thanks",
    "thankful",
    "grateful",
    "appreciate",
    "appreciation",
    "regard",
    "regards",
    "sincerely",
    "best",
    "warm",
    "kind",
    "consider",
    "consideration",
    "look",
    "looking",
    "learn",
    "learning",
    "role",
    "position",
    "opening",
    "job",
    "posting",
    "company",
    "organization",
    "team",
    "group",
    "background",
    "further",
    "soon",
    "attach",
    "attached",
    "review",
    "convenience",
    "qualification",
    "qualifications",
    "contribute",
    "contribution",
    "help",
    "bring",
    "offer",
    "add",
    "join",
    "joining",
    "fit",
    "part",
    "letter",
    "cover",
    "application",
    "apply",
    "applying",
    "applicant",
    "candidate",
    "career",
    "work",
    "working",
    "hire",
    "hiring",
    "resume",
    "seek",
    "seeking",
    "love",
    "enjoy",
    "want",
    "wish",
    "like",
    "believe",
    "think",
    "feel",
    "know",
    "find",
    "see",
    "use",
    "way",
    "thing",
    "things",
    "something",
    "anything",
    "everything",
    "someone",
    "anyone",
    "everyone",
    "lot",
    "good",
    "great",
    "strong",
    "solid",
    "excellent",
    "effective",
    "successful",
    "valuable",
    "relevant",
    "ideal",
    "right",
    "real",
    "true",
    "sure",
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cover::CoverLetter;
    use crate::dataset::types::{Contact, SkillCategory};
    use crate::jd::{Importance, JdSkill, RemotePolicy, Seniority};
    use crate::tailor::{
        BuildId, JdId, SkillsSection, TailoredBullet, TailoredResume, TailoredRole,
    };
    use chrono::Utc;

    // --- fixtures ---------------------------------------------------------

    fn contact() -> Contact {
        Contact {
            full_name: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            phone: None,
            location: None,
            links: Vec::new(),
        }
    }

    /// A résumé whose one bullet names distinctive, checkable content:
    /// Contoso, a deployment pipeline, reliability work, and the figure 12.
    fn resume() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("b1".into()),
            jd_id: JdId("jd1".into()),
            generated_at: Utc::now(),
            contact: contact(),
            target_title: Some("Staff Engineer".into()),
            summary: "Engineering leader focused on platform reliability.".into(),
            roles: vec![TailoredRole {
                id: crate::dataset::types::RoleId("role-1".into()),
                company: "Contoso".into(),
                title: "Director of Engineering".into(),
                start: crate::dataset::types::YearMonth {
                    year: 2020,
                    month: 3,
                },
                end: None,
                location: None,
                bullets: vec![TailoredBullet {
                    source_id: crate::dataset::types::BulletId("bullet-1".into()),
                    text:
                        "Rebuilt the deployment pipeline and led reliability work for 12 services"
                            .into(),
                }],
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec!["Distributed systems".into(), "Incident response".into()],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    fn jd() -> JobRequirements {
        JobRequirements {
            company: "Acme".into(),
            title: "Platform Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: Vec::new(),
            required_skills: vec![JdSkill {
                name: "Kubernetes".into(),
                category: SkillCategory::Hard,
                importance: Importance::Critical,
                context_phrases: Vec::new(),
            }],
            preferred_skills: Vec::new(),
            responsibilities: vec!["Own platform reliability at scale".into()],
            ats_phrases: Vec::new(),
            raw_text: String::new(),
            source_url: None,
        }
    }

    /// A letter carrying exactly the paragraphs handed in — greeting and
    /// sign-off are set to fixed code-filled values a real assembly would
    /// use, and are never part of `paragraphs`.
    fn letter(paragraphs: &[&str]) -> CoverLetter {
        CoverLetter {
            contact: contact(),
            company: "Acme".into(),
            title: "Platform Engineer".into(),
            greeting: "Dear Acme hiring team,".into(),
            paragraphs: paragraphs.iter().map(|p| p.to_string()).collect(),
            signoff: "Ada Lovelace".into(),
        }
    }

    /// The single stemmed token `keyword_key` reduces a one-word term to —
    /// used so assertions don't hard-code the stemmer's output.
    fn token(word: &str) -> String {
        keyword_key(word).into_iter().next().expect("one token")
    }

    // --- tests ------------------------------------------------------------

    #[test]
    fn a_purely_connective_paragraph_is_exempt() {
        let letter = letter(&[
            "I'd welcome the opportunity to discuss how my background could contribute to your team.",
        ]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(
            p.status,
            CoverParagraphStatus::Exempt,
            "benign framing must not be flagged; unbacked tokens were {:?}",
            p.unbacked_tokens
        );
        assert!(p.unbacked_tokens.is_empty());
        assert!(p.unbacked_digits.is_empty());
    }

    #[test]
    fn a_paraphrase_of_a_recorded_bullet_is_grounded() {
        // Same facts as the résumé bullet, restructured into a different
        // sentence. It keeps the distinctive nouns a cover letter can't
        // reword away (Contoso, deployment pipeline, reliability) and the
        // recorded figure 12; only the connective framing differs.
        let letter = letter(&[
            "At Contoso I led the reliability work for 12 services and rebuilt their deployment pipeline.",
        ]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(
            p.status,
            CoverParagraphStatus::Grounded,
            "unbacked tokens were {:?}, digits {:?}",
            p.unbacked_tokens,
            p.unbacked_digits
        );
    }

    #[test]
    fn a_paragraph_echoing_the_posting_language_is_grounded() {
        // "platform reliability at scale" is the posting's own phrasing and
        // "Kubernetes" its required skill; neither is on the résumé. The
        // corpus includes the JD, so echoing it grounds rather than flags.
        let letter =
            letter(&["I want to own platform reliability at scale, and I work in Kubernetes."]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(
            p.status,
            CoverParagraphStatus::Grounded,
            "unbacked tokens were {:?}",
            p.unbacked_tokens
        );
    }

    #[test]
    fn a_paragraph_inventing_a_number_is_unrecorded_with_that_digit() {
        // Every word is backed (the bullet has "improved"? no — reuse the
        // corpus's own words: platform, reliability, services, 12), and the
        // only new thing is the figure 63, which is nowhere in the corpus.
        let letter =
            letter(&["I improved platform reliability across 12 services, cutting incidents 63."]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert_eq!(p.unbacked_digits, vec!["63".to_string()]);
        // 12 is a résumé figure, so it is not flagged.
        assert!(!p.unbacked_digits.contains(&"12".to_string()));
        // The invented figure is reported as a digit, never doubled up as a
        // bare-number token.
        assert!(!p.unbacked_tokens.contains(&"63".to_string()));
    }

    #[test]
    fn a_paragraph_inventing_a_skill_is_unrecorded_with_that_token() {
        // "Rust" appears in neither the résumé, the posting, nor a brief.
        let letter = letter(&["I led platform reliability work using Rust for 12 services."]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert!(
            p.unbacked_tokens.contains(&token("Rust")),
            "expected {:?} in {:?}",
            token("Rust"),
            p.unbacked_tokens
        );
        assert!(p.unbacked_digits.is_empty());
    }

    #[test]
    fn a_paragraph_inventing_an_employer_is_unrecorded_with_that_token() {
        let letter = letter(&["Before this I spent time at Initech on platform reliability."]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert!(
            p.unbacked_tokens.contains(&token("Initech")),
            "expected {:?} in {:?}",
            token("Initech"),
            p.unbacked_tokens
        );
    }

    #[test]
    fn a_paragraph_grounded_only_in_the_brief_is_grounded() {
        // The motivating case: a fact the résumé and posting don't carry,
        // that the candidate recorded in the interview brief. "ChessCoach"
        // (a side project) exists nowhere but the brief; a paragraph built
        // from it must be grounded, proving the brief is part of the corpus.
        let brief = CoverBrief {
            emphasis: vec!["my side project ChessCoach, a chess training app".into()],
            ..CoverBrief::default()
        };
        let letter = letter(&["On my own time I built ChessCoach, a chess training app."]);

        // Without the brief the same paragraph is unrecorded...
        let without = check_cover_provenance(&letter, &resume(), &jd(), None);
        assert_eq!(
            without.paragraphs[0].status,
            CoverParagraphStatus::Unrecorded
        );
        assert!(
            without.paragraphs[0]
                .unbacked_tokens
                .contains(&token("ChessCoach"))
        );

        // ...and with it, it is grounded.
        let with = check_cover_provenance(&letter, &resume(), &jd(), Some(&brief));
        assert_eq!(
            with.paragraphs[0].status,
            CoverParagraphStatus::Grounded,
            "unbacked tokens were {:?}",
            with.paragraphs[0].unbacked_tokens
        );
    }

    #[test]
    fn a_term_only_a_voice_sample_could_carry_is_never_grounded() {
        // Voice samples are not a parameter of `check_cover_provenance`, so
        // a term that in a real flow would live only in the candidate's
        // writing samples (here, "kitesurfing") can never reach the corpus.
        // Proves the exclusion structurally: there is no argument through
        // which such a term could ground a paragraph. It appears nowhere in
        // the résumé, posting, or brief, so it is flagged.
        let letter = letter(&["I picked up kitesurfing over the summer."]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert!(
            p.unbacked_tokens.contains(&token("kitesurfing")),
            "expected {:?} in {:?}",
            token("kitesurfing"),
            p.unbacked_tokens
        );
    }

    #[test]
    fn no_brief_still_classifies_against_resume_and_jd() {
        // A grounded (posting-echo), an exempt, and an unrecorded paragraph
        // all resolve correctly when brief is None.
        let letter = letter(&[
            "I work in Kubernetes on platform reliability at scale.",
            "I'd be glad to talk further whenever it suits you.",
            "I also shipped a mission to Mars last year.",
        ]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        assert_eq!(report.paragraphs[0].status, CoverParagraphStatus::Grounded);
        assert_eq!(report.paragraphs[1].status, CoverParagraphStatus::Exempt);
        assert_eq!(
            report.paragraphs[2].status,
            CoverParagraphStatus::Unrecorded
        );
    }

    #[test]
    fn only_body_paragraphs_are_classified_never_greeting_or_signoff() {
        // The report has exactly one entry per body paragraph — the
        // greeting ("Dear Acme hiring team,") and sign-off ("Ada Lovelace")
        // are code-filled and not part of `paragraphs`, so they are never
        // counted or classified.
        let letter = letter(&["First body paragraph.", "Second body paragraph."]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        assert_eq!(report.paragraphs.len(), letter.paragraphs.len());
        assert_eq!(report.paragraphs.len(), 2);
        for p in &report.paragraphs {
            assert!(!p.text.contains("Dear Acme"));
            assert_ne!(p.text, "Ada Lovelace");
        }
    }

    #[test]
    fn unbacked_lists_are_sorted_and_report_is_serializable() {
        // Two invented tokens come back sorted, and the whole report round
        // trips through JSON (it crosses into the browser later).
        let letter = letter(&["I used Zig and Haskell on the platform."]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        let mut sorted = p.unbacked_tokens.clone();
        sorted.sort();
        assert_eq!(p.unbacked_tokens, sorted, "unbacked tokens must be sorted");

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"unrecorded\""));
        let back: CoverProvenanceReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn a_jd_requirement_number_does_not_ground_a_personal_history_claim() {
        // Regression for the digit-corpus split. The posting states "5+
        // years" as a role requirement, but nothing the candidate recorded
        // (résumé or brief) contains a "5". A paragraph that asserts "5
        // years of experience" as personal history must be flagged, not
        // grounded off the posting's stated requirement — a JD's numbers
        // describe the role, never what the candidate has done.
        let mut jd = jd();
        jd.responsibilities = vec!["Bring 5+ years of platform engineering experience".into()];
        let letter = letter(&["I have 5 years of experience with Kubernetes."]);
        let report = check_cover_provenance(&letter, &resume(), &jd, None);
        let p = &report.paragraphs[0];
        assert_eq!(
            p.status,
            CoverParagraphStatus::Unrecorded,
            "unbacked tokens {:?}, digits {:?}",
            p.unbacked_tokens,
            p.unbacked_digits
        );
        assert_eq!(p.unbacked_digits, vec!["5".to_string()]);
        // The role language the paragraph echoes ("years", "Kubernetes") is
        // still legitimate as tokens — only the posting's number is withheld
        // from the evidence set.
        assert!(
            p.unbacked_tokens.is_empty(),
            "only the digit should be unbacked, got tokens {:?}",
            p.unbacked_tokens
        );
    }

    #[test]
    fn ordinary_signoff_language_is_not_flagged() {
        // Regression for the over-narrow filler list. Routine
        // connective/politeness sentences carry no fabrication and must not
        // read as unrecorded, or a real invention would be buried in noise
        // from sign-off boilerplate.
        let benign = [
            "I look forward to hearing from you soon.",
            "Please find my resume attached for your review.",
            "I would be glad to discuss my qualifications further at your convenience.",
        ];
        for sentence in benign {
            let letter = letter(&[sentence]);
            let report = check_cover_provenance(&letter, &resume(), &jd(), None);
            let p = &report.paragraphs[0];
            assert_ne!(
                p.status,
                CoverParagraphStatus::Unrecorded,
                "{sentence:?} wrongly flagged; tokens {:?}",
                p.unbacked_tokens
            );
        }

        // But "leadership" is deliberately kept off the filler list: a
        // candidate asserting leadership experience is a real claim, so if
        // nothing backs it the sentence still (correctly) flags. This locks
        // in that the filler additions didn't get greedy and start hiding
        // claim words.
        let letter = letter(&[
            "I would love to bring my background in engineering leadership to your growing team.",
        ]);
        let report = check_cover_provenance(&letter, &resume(), &jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert!(
            p.unbacked_tokens.contains(&token("leadership")),
            "expected leadership flagged, got {:?}",
            p.unbacked_tokens
        );
    }

    // --- founding-engineer fixture for the real-prose regression ----------

    /// A résumé for the scenario the real failure came from: a founding
    /// engineer who scaled a team on a FINRA-regulated platform, working in
    /// TypeScript and PostgreSQL with CI/CD and AI-assisted development. The
    /// corpus this produces carries the *facts* the reconstructed paragraphs
    /// below are built from, so anything that still flags is genuinely
    /// unrecorded, not a rewording of a recorded fact.
    fn founding_resume() -> TailoredResume {
        TailoredResume {
            build_id: BuildId("b2".into()),
            jd_id: JdId("jd2".into()),
            generated_at: Utc::now(),
            contact: contact(),
            target_title: Some("Founding Engineer".into()),
            summary: "Founding engineer who scaled the engineering team on a FINRA regulated \
                      platform."
                .into(),
            roles: vec![TailoredRole {
                id: crate::dataset::types::RoleId("role-1".into()),
                company: "Northwind".into(),
                title: "Founding Engineer".into(),
                start: crate::dataset::types::YearMonth {
                    year: 2019,
                    month: 1,
                },
                end: None,
                location: None,
                bullets: vec![
                    TailoredBullet {
                        source_id: crate::dataset::types::BulletId("bullet-1".into()),
                        text: "Built code review and CI/CD releases across regulated systems."
                            .into(),
                    },
                    TailoredBullet {
                        source_id: crate::dataset::types::BulletId("bullet-2".into()),
                        text: "Introduced AI assisted development with TypeScript and PostgreSQL \
                               for a team of 20 engineers."
                            .into(),
                    },
                ],
            }],
            education: Vec::new(),
            skills_section: SkillsSection {
                skills: vec![
                    "Distributed systems".into(),
                    "TypeScript".into(),
                    "PostgreSQL".into(),
                ],
            },
            projects: Vec::new(),
            achievements: Vec::new(),
            certifications: Vec::new(),
        }
    }

    /// The posting for that scenario. `responsibilities` name the platform
    /// but no payments language, so the payments family is absent from the
    /// corpus unless a test adds it deliberately.
    fn founding_jd() -> JobRequirements {
        JobRequirements {
            company: "Northwind".into(),
            title: "Founding Engineer".into(),
            seniority: Seniority::Senior,
            location: None,
            remote: RemotePolicy::Remote,
            domain_keywords: Vec::new(),
            required_skills: vec![
                JdSkill {
                    name: "TypeScript".into(),
                    category: SkillCategory::Hard,
                    importance: Importance::Critical,
                    context_phrases: Vec::new(),
                },
                JdSkill {
                    name: "PostgreSQL".into(),
                    category: SkillCategory::Hard,
                    importance: Importance::Required,
                    context_phrases: Vec::new(),
                },
            ],
            preferred_skills: Vec::new(),
            responsibilities: vec!["Own the reliability of the core platform".into()],
            ats_phrases: Vec::new(),
            raw_text: String::new(),
            source_url: None,
        }
    }

    #[test]
    fn real_generated_prose_is_not_wholesale_flagged() {
        // The regression that motivated widening the filler. These four
        // paragraphs rebuild a real generated letter's vocabulary — the same
        // connective and evaluative words that came back flagged before
        // ("rigor", "clarity", "traceable", "auditable", "crisis",
        // "foundational", "systematically", "partnership", "when", ...) —
        // over the facts the founding-engineer corpus records. Every one
        // must now land Grounded (they each state backed facts); none may be
        // Unrecorded. The old dozen-word filler flagged all four at 100%.
        let paragraphs = [
            "As a founding engineer I ran the engineering team through many challenges, \
             delivering foundational work with clarity and without compromising the platform. \
             That ability to deliver alone was the rigor the team ran on.",
            "The FINRA regulated platform forced rigor into every release from the start. Code \
             review had to be traceable and releases auditable, so nothing could be ignored. We \
             systematically built that in rather than retrofit it, because a regulated platform \
             could not ignore a crisis.",
            "Across the last several roles I grew that engineering team meaningfully. When the \
             platform needed it, nothing was peripheral.",
            "AI assisted development was exactly the critical, multifaceted partnership the \
             platform needed, so the team could run in parallel.",
        ];
        let letter = letter(&paragraphs);
        let report = check_cover_provenance(&letter, &founding_resume(), &founding_jd(), None);
        for (i, p) in report.paragraphs.iter().enumerate() {
            assert_eq!(
                p.status,
                CoverParagraphStatus::Grounded,
                "paragraph {i} wrongly flagged; unbacked tokens {:?}, digits {:?}",
                p.unbacked_tokens,
                p.unbacked_digits,
            );
        }
    }

    #[test]
    fn a_genuinely_fabricated_claim_is_still_caught_after_widening() {
        // The single most important guard on this change: proving the
        // broader filler did not swallow real fabrications. This paragraph
        // invents an employer (Globex), a technology (Fortran), and a metric
        // (87) that appear nowhere in the corpus. All three are specific
        // words, so none is filler, and each must still be flagged.
        let letter = letter(&[
            "I built the settlement system at Globex with Fortran, cutting reconciliation latency \
             by 87 percent.",
        ]);
        let report = check_cover_provenance(&letter, &founding_resume(), &founding_jd(), None);
        let p = &report.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert!(
            p.unbacked_tokens.contains(&token("Globex")),
            "invented employer must still flag; got {:?}",
            p.unbacked_tokens
        );
        assert!(
            p.unbacked_tokens.contains(&token("Fortran")),
            "invented technology must still flag; got {:?}",
            p.unbacked_tokens
        );
        assert!(
            p.unbacked_digits.contains(&"87".to_string()),
            "invented metric must still flag; got {:?}",
            p.unbacked_digits
        );
    }

    #[test]
    fn payments_language_grounds_only_when_the_corpus_actually_carries_it() {
        // The "pay" investigation from the real failure. keyword_key stems
        // "payments", "payment", and "pay" all to the same root, so there is
        // no tokenization mismatch to fix: when the posting actually names
        // payments, a paragraph echoing it grounds through the corpus. The
        // real letter's lone "pay" flag was therefore correct — that corpus
        // simply carried no payments language — and "pay"/"payments" is a
        // genuine business-domain term that must never be filtered as
        // filler, so an unbacked payments claim stays flagged.
        let paragraph = "I want to own the payments platform.";
        assert_eq!(token("payments"), token("pay"), "the two must share a stem");

        // Posting names the platform but no payments: the claim is unbacked.
        let without = check_cover_provenance(
            &letter(&[paragraph]),
            &founding_resume(),
            &founding_jd(),
            None,
        );
        let p = &without.paragraphs[0];
        assert_eq!(p.status, CoverParagraphStatus::Unrecorded);
        assert!(
            p.unbacked_tokens.contains(&token("payments")),
            "an unbacked payments claim must flag; got {:?}",
            p.unbacked_tokens
        );

        // Posting names payments: the same paragraph now grounds, with no
        // special-casing — the shared stem matches through the corpus.
        let mut jd_pay = founding_jd();
        jd_pay.responsibilities = vec!["Scale the payments platform end to end".into()];
        let with = check_cover_provenance(&letter(&[paragraph]), &founding_resume(), &jd_pay, None);
        assert_eq!(
            with.paragraphs[0].status,
            CoverParagraphStatus::Grounded,
            "payments language in the posting must ground the echo; got {:?}",
            with.paragraphs[0].unbacked_tokens
        );
    }
}
