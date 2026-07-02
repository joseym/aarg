/** View-model helpers for the tailoring workspace. Pure functions only — no
 *  Angular, no I/O — so the container stays a thin coordinator and the child
 *  components stay presentational. Everything here maps the shared wire models
 *  (`ProvenanceReport`, `Objection`, `VariantPayload`, …) into the shapes the
 *  preview / rail / coverage components render.
 *
 *  NOTE (flagged, not fixed): `BuildDetail` (models/build.ts) exposes no human
 *  `VariantPayload` and no `dismissed_objections` typing on `metadata`; and
 *  `Objection` carries neither a wire id nor the verbatim flagged line. We read
 *  those defensively here and derive a stable id from `(target, kind)`. */

import type {
  Objection,
  ObjectionKind,
  ObjectionTarget,
  ObjectionScope,
} from '../../models';
import type { LineLocation, LineProvenance, SourceRef, ProvenanceReport } from '../../models';
import type { ResumeDataset } from '../../models';
import type { VariantPayload } from '../../models';
import type { GapReport } from '../../models';

// ── provenance ──────────────────────────────────────────────────────────

/** Per-line provenance status as the preview renders it. `edited` is a local
 *  overlay (the user changed the line) that the deterministic core never emits. */
export type LineStatus = 'verbatim' | 'grounded' | 'unrecorded' | 'edited';

/** A stable string key for a rendered line, shared by the provenance report and
 *  the rendered payload so a line's status can be looked up positionally. */
export function locationKey(loc: LineLocation): string {
  if (loc === 'summary') return 'summary';
  if ('role_bullet' in loc) {
    return `bullet:${loc.role_bullet.role_id}:${loc.role_bullet.bullet_index}`;
  }
  return `skill:${loc.skill.index}`;
}

/** Index a provenance report by line key for O(1) lookup while rendering. */
export function provenanceIndex(report: ProvenanceReport | null): Map<string, LineProvenance> {
  const map = new Map<string, LineProvenance>();
  for (const line of report?.lines ?? []) {
    map.set(locationKey(line.location), line);
  }
  return map;
}

/** Human label for the dataset item a line resolved to (for the popover). Uses
 *  `textContent`-safe strings only — the caller renders them via interpolation. */
export function resolveSource(source: SourceRef | null, dataset: ResumeDataset | null): string {
  if (!source) return 'your recorded evidence';
  if (source === 'summary') return 'your saved summary';
  if ('bullet' in source) {
    const id = source.bullet.id;
    for (const role of dataset?.roles ?? []) {
      for (const b of role.bullets) {
        if (b.id === id) return `${role.company} · ${truncate(b.text, 80)}`;
      }
    }
    return `recorded bullet ${id}`;
  }
  // skill
  const id = source.skill.id;
  const skill = dataset?.skills?.skills?.find((s) => s.id === id);
  return skill ? skill.canonical_name : `recorded skill ${id}`;
}

function truncate(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

/** The popover copy for one line, given its status and best match. */
export function provenanceCopy(
  status: LineStatus,
  line: LineProvenance | undefined,
  dataset: ResumeDataset | null,
): { label: string; text: string } {
  switch (status) {
    case 'edited':
      return { label: 'Provenance', text: 'your own edit' };
    case 'verbatim':
      return { label: 'Traces to', text: `verbatim from ${resolveSource(line?.best_match?.source ?? null, dataset)}` };
    case 'grounded':
      return { label: 'Closest match', text: resolveSource(line?.best_match?.source ?? null, dataset) };
    case 'unrecorded':
      return { label: 'Needs review', text: 'not yet traced to your evidence — confirm before it lands' };
  }
}

// ── preview model (from the human VariantPayload) ───────────────────────

export interface PreviewLine {
  key: string;
  text: string;
  status: LineStatus | null;
  prov: { label: string; text: string } | null;
}

export interface PreviewRole {
  id: string;
  title: string;
  company: string;
  dates: string;
  bullets: PreviewLine[];
}

export interface PreviewModel {
  name: string;
  targetTitle: string;
  contact: string;
  summary: PreviewLine;
  roles: PreviewRole[];
  skills: PreviewLine[];
}

/** Build the preview model from the human variant payload, overlaying the
 *  provenance index and any local edits. Edited lines win the status. */
export function buildPreviewModel(
  payload: VariantPayload,
  provMap: Map<string, LineProvenance>,
  edits: Record<string, string>,
  dataset: ResumeDataset | null,
): PreviewModel {
  const line = (key: string, original: string): PreviewLine => {
    const edited = Object.prototype.hasOwnProperty.call(edits, key);
    const prov = provMap.get(key);
    const status: LineStatus | null = edited ? 'edited' : (prov?.status ?? null);
    return {
      key,
      text: edited ? edits[key] : original,
      status,
      prov: status ? provenanceCopy(status, prov, dataset) : null,
    };
  };

  return {
    name: payload.contact?.full_name ?? '',
    targetTitle: payload.target_title ?? '',
    contact: contactLine(payload),
    summary: line('summary', payload.summary ?? ''),
    roles: (payload.roles ?? []).map((r) => ({
      id: r.id,
      title: r.title,
      company: r.company,
      dates: `${r.start ?? ''}${r.end ? ' — ' + r.end : r.start ? ' — Present' : ''}`,
      bullets: r.bullets.map((b, i) => line(`bullet:${r.id}:${i}`, b.text)),
    })),
    skills: (payload.skills_section?.skills ?? []).map((s, i) => line(`skill:${i}`, s)),
  };
}

function contactLine(payload: VariantPayload): string {
  const c = payload.contact;
  if (!c) return '';
  return [c.location, c.email, ...(c.links ?? []).map((l) => l.url)].filter(Boolean).join(' · ');
}

// ── objections ──────────────────────────────────────────────────────────

export type ObjectionType = 'unsupported' | 'metric' | 'weak' | 'skills' | 'layout' | 'overall';
export type CopilotKind = 'strengthen' | 'metric' | 'summary' | 'skills' | 'layout';
export type TriageStatus = 'open' | 'accepted' | 'left';

export interface ObjectionVM {
  /** Stable id derived from `(target, kind)` — Objection has none on the wire. */
  id: string;
  objection: Objection;
  type: ObjectionType;
  typeLabel: string;
  targetLabel: string;
  flaggedText: string | null;
  message: string;
  suggestion: string | null;
  severity: Objection['severity'];
  copilot: CopilotKind;
}

const TYPE_LABEL: Record<ObjectionType, string> = {
  unsupported: 'Unsupported claim',
  metric: 'Missing metric',
  weak: 'Weak wording',
  skills: 'Skills gap',
  layout: 'Layout',
  overall: 'Overall',
};

/** Map the domain `ObjectionKind` onto the prototype's five display types
 *  (each paired with a token hue) and the copilot a refine action would open. */
function classifyKind(kind: ObjectionKind): { type: ObjectionType; copilot: CopilotKind } {
  switch (kind) {
    case 'unsupported_claim':
      return { type: 'unsupported', copilot: 'strengthen' };
    case 'no_metric':
      return { type: 'metric', copilot: 'metric' };
    case 'vague_verb':
    case 'generic_phrasing':
      return { type: 'weak', copilot: 'strengthen' };
    case 'jd_mismatch':
      return { type: 'skills', copilot: 'skills' };
    case 'layout_dense':
      return { type: 'layout', copilot: 'layout' };
    default:
      return { type: 'overall', copilot: 'strengthen' };
  }
}

export function targetKey(target: ObjectionTarget): string {
  return typeof target === 'string' ? target : `bullet:${target.bullet}`;
}

export function objectionId(o: Objection): string {
  return `${targetKey(o.target)}::${o.kind}`;
}

function targetLabel(target: ObjectionTarget, dataset: ResumeDataset | null): string {
  if (target === 'summary') return 'Summary';
  if (target === 'skills_section') return 'Skills';
  if (target === 'layout') return 'Layout';
  if (target === 'overall') return 'Overall';
  // bullet: try to name the role it belongs to
  for (const role of dataset?.roles ?? []) {
    if (role.bullets.some((b) => b.id === target.bullet)) {
      return `${role.company} · bullet`;
    }
  }
  return 'Bullet';
}

/** Resolve an objection's target to the actual flagged line, if we can find it. */
function flaggedText(
  target: ObjectionTarget,
  payload: VariantPayload | null,
  dataset: ResumeDataset | null,
): string | null {
  if (target === 'summary') return payload?.summary ?? dataset?.summary ?? null;
  if (typeof target === 'object') {
    for (const role of dataset?.roles ?? []) {
      const b = role.bullets.find((x) => x.id === target.bullet);
      if (b) return b.text;
    }
    for (const role of payload?.roles ?? []) {
      const b = role.bullets.find((x) => x.source_id === target.bullet);
      if (b) return b.text;
    }
  }
  return null;
}

/** Prefer a layout copilot when the objection is variant-only presentation. */
function copilotForScope(scope: ObjectionScope, fallback: CopilotKind): CopilotKind {
  return typeof scope === 'object' ? 'layout' : fallback;
}

export function buildObjectionVMs(
  objections: Objection[],
  payload: VariantPayload | null,
  dataset: ResumeDataset | null,
): ObjectionVM[] {
  return objections.map((o) => {
    const { type, copilot } = classifyKind(o.kind);
    return {
      id: objectionId(o),
      objection: o,
      type,
      typeLabel: TYPE_LABEL[type],
      targetLabel: targetLabel(o.target, dataset),
      flaggedText: flaggedText(o.target, payload, dataset),
      message: o.message,
      suggestion: o.suggestion,
      severity: o.severity,
      copilot: copilotForScope(o.scope, copilot),
    };
  });
}

// ── coverage map ────────────────────────────────────────────────────────

export type CoverageState = 'matched' | 'weak' | 'unmatched';

export interface CoverageRow {
  name: string;
  state: CoverageState;
  evidence: string | null;
}

export function buildCoverageRows(gap: GapReport | null): CoverageRow[] {
  if (!gap) return [];
  return [
    ...gap.matched.map((m) => ({
      name: m.jd_skill.name,
      state: 'matched' as const,
      evidence: `${m.dataset_name}${m.semantic ? ' (semantic match)' : ''}`,
    })),
    ...gap.weak.map((m) => ({
      name: m.jd_skill.name,
      state: 'weak' as const,
      evidence: `partial: ${m.dataset_name}`,
    })),
    ...gap.unknown.map((s) => ({
      name: s.name,
      state: 'unmatched' as const,
      evidence: null,
    })),
  ];
}

// ── scoring ─────────────────────────────────────────────────────────────

/** Legibility band for a 0..1 score (never the sole signal — always labelled). */
export function band(score01: number): 'ok' | 'warn' | 'bad' {
  const pct = score01 * 100;
  return pct >= 80 ? 'ok' : pct >= 60 ? 'warn' : 'bad';
}

export function pct(v: number | null | undefined): number {
  return Math.round((v ?? 0) * 100);
}
