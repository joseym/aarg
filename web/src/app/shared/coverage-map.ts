import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
  signal,
} from '@angular/core';

import type {
  GapMatch,
  GapReport,
  JobRequirements,
  SkillImportance,
} from '../models';

export type ReqState = 'exact' | 'semantic' | 'gap';

/** One coverage-table row: a JD requirement paired with the dataset evidence (if
 *  any) that backs it. `intent` is the deep-link hint the tailoring screen reads. */
export interface ReqRow {
  name: string;
  importance: SkillImportance;
  category: string;
  context: string;
  state: ReqState;
  mark: string;
  tag: string;
  evidence: string;
  action: string;
  intent: 'matched' | 'semantic' | 'gap';
}

export type CovFilter = 'all' | 'exact' | 'semantic' | 'gap';

/** The requirement ↔ evidence coverage table, shared by the build overview and
 *  the tailoring workspace. Takes the raw JD + gap report, cross-references them
 *  into rows, and offers an in-table filter. A row's action emits {@link act}
 *  (the host decides where "Refine / Strengthen / Fill the gap" navigates). */
@Component({
  selector: 'app-coverage-map',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (rows().length === 0) {
      <div class="cov-map anim-fade">
        <div class="cov-empty">
          This build has no parsed requirements to map yet.
        </div>
      </div>
    } @else {
      <div class="cov-map anim-fade">
        <div class="cm-head">
          <div><span>Job requirement</span></div>
          <div>
            <span>Your background</span>
            <span class="legend">
              <span class="lg"><i style="background: var(--success)"></i>exact</span>
              <span class="lg"><i style="background: var(--warn)"></i>semantic</span>
              <span class="lg"><i style="background: var(--danger)"></i>gap</span>
            </span>
          </div>
        </div>

        <div class="cov-filter" role="group" aria-label="Filter requirements">
          <span class="fl-label">Filter</span>
          <button type="button" class="cov-chip" [class.on]="covFilter() === 'all'" (click)="setFilter('all')">
            All <span class="fl-n">{{ counts().all }}</span>
          </button>
          <button type="button" class="cov-chip" data-t="exact" [class.on]="covFilter() === 'exact'" (click)="setFilter('exact')">
            <span class="fl-dot"></span>Matched <span class="fl-n">{{ counts().exact }}</span>
          </button>
          <button type="button" class="cov-chip" data-t="semantic" [class.on]="covFilter() === 'semantic'" (click)="setFilter('semantic')">
            <span class="fl-dot"></span>Weak <span class="fl-n">{{ counts().semantic }}</span>
          </button>
          <button type="button" class="cov-chip" data-t="gap" [class.on]="covFilter() === 'gap'" (click)="setFilter('gap')">
            <span class="fl-dot"></span>Gaps <span class="fl-n">{{ counts().gap }}</span>
          </button>
        </div>

        @for (r of visibleRows(); track r.name) {
          <div class="req-row" [class]="r.state" [attr.data-t]="r.state">
            <div class="rq-left">
              <div class="req-top">
                <span class="req-name">{{ r.name }}</span>
                <span class="imp" [attr.data-imp]="r.importance">{{ r.importance }}</span>
              </div>
              @if (r.context) {
                <div class="req-ctx">{{ r.context }}</div>
              }
            </div>
            <div class="rq-right">
              <div class="ev">
                <span class="ev-mark" aria-hidden="true">{{ r.mark }}</span>
                <span class="ev-name">{{ r.evidence }}</span>
              </div>
              <div class="ev-tag">{{ r.tag }}</div>
              <button
                type="button"
                class="cov-act"
                (click)="act.emit({ name: r.name, intent: r.intent })"
                [attr.title]="'Open targeted tailoring for ' + r.name"
              >
                {{ r.action }}
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                  <path d="M5 12h14M13 6l6 6-6 6" />
                </svg>
              </button>
            </div>
          </div>
        } @empty {
          <div class="cov-empty">{{ emptyFilterNote() }}</div>
        }

        <div class="cm-foot">
          <span style="color: var(--success)"><b style="color: var(--success)">{{ counts().exact }}</b> exact</span>
          <span style="color: var(--warn)"><b style="color: var(--warn)">{{ counts().semantic }}</b> semantic</span>
          <span style="color: var(--danger)"><b style="color: var(--danger)">{{ counts().gap }}</b> gaps</span>
        </div>
      </div>
    }
  `,
  styles: `
    :host { display: block; }

    .cov-map { border: 1px solid var(--border); border-radius: var(--radius-lg); overflow: hidden; background: var(--surface); }
    .cm-head { display: grid; grid-template-columns: 1.15fr 1fr; }
    .cm-head > div { padding: 12px 20px; font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--muted); display: flex; align-items: center; justify-content: space-between; gap: 10px; background: var(--surface-2); border-bottom: 1px solid var(--border); }
    .cm-head > div:first-child { border-right: 1px solid var(--border); }
    .cm-head .legend { display: flex; gap: 12px; text-transform: none; letter-spacing: 0; font-size: 10.5px; }
    .cm-head .lg { display: inline-flex; align-items: center; gap: 5px; color: var(--faint); }
    .cm-head .lg i { width: 9px; height: 9px; border-radius: 2px; }

    .cov-filter { display: flex; align-items: center; gap: 7px; padding: 11px 16px; border-bottom: 1px solid var(--border); background: var(--surface-2); flex-wrap: wrap; }
    .cov-filter .fl-label { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--faint); margin-right: 2px; }
    .cov-chip { display: inline-flex; align-items: center; gap: 7px; padding: 5px 11px; border-radius: 999px; border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 12.5px; color: var(--muted); cursor: pointer; transition: border-color 0.12s, color 0.12s, background 0.12s; }
    .cov-chip:hover { border-color: var(--fg); color: var(--fg); }
    .cov-chip .fl-dot { width: 8px; height: 8px; border-radius: 2px; background: var(--faint); }
    .cov-chip[data-t='exact'] .fl-dot { background: var(--success); }
    .cov-chip[data-t='semantic'] .fl-dot { background: var(--warn); }
    .cov-chip[data-t='gap'] .fl-dot { background: var(--danger); }
    .cov-chip .fl-n { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 11px; color: var(--faint); }
    .cov-chip.on { border-color: var(--accent); color: var(--accent); background: var(--accent-soft); }
    .cov-chip.on .fl-n { color: var(--accent); }

    .req-row { display: grid; grid-template-columns: 1.15fr 1fr; border-bottom: 1px solid color-mix(in oklch, var(--border) 70%, transparent); }
    .req-row:last-of-type { border-bottom: 0; }
    .rq-left { padding: 14px 18px; border-right: 1px solid color-mix(in oklch, var(--border) 70%, transparent); }
    .rq-right { padding: 14px 18px; display: flex; flex-direction: column; justify-content: center; gap: 4px; }
    .req-top { display: flex; align-items: center; gap: 9px; flex-wrap: wrap; }
    .req-name { font-family: var(--font-display); font-size: 15.5px; line-height: 1.25; }
    .imp { font-family: var(--font-mono); font-size: 9.5px; letter-spacing: 0.06em; text-transform: uppercase; padding: 2px 7px; border-radius: 999px; border: 1px solid var(--border); color: var(--muted); }
    .imp[data-imp='critical'] { color: var(--danger); border-color: color-mix(in oklch, var(--danger) 35%, var(--border)); }
    .imp[data-imp='required'] { color: var(--fg); }
    .imp[data-imp='preferred'], .imp[data-imp='optional'] { color: var(--faint); }
    .req-ctx { font-size: 12.5px; color: var(--muted); margin-top: 6px; line-height: 1.45; }

    .ev { display: flex; align-items: baseline; gap: 8px; }
    .ev-mark { font-family: var(--font-mono); font-size: 13px; flex-shrink: 0; line-height: 1.4; }
    .ev-name { font-size: 14px; }
    .ev-tag { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.04em; text-transform: uppercase; color: var(--faint); margin-top: 2px; }
    .req-row.exact .rq-right { background: color-mix(in oklch, var(--success) 7%, transparent); }
    .req-row.exact .ev-mark { color: var(--success); }
    .req-row.semantic .rq-right { background: color-mix(in oklch, var(--warn) 8%, transparent); }
    .req-row.semantic .ev-mark { color: var(--warn); }
    .req-row.gap .rq-right { background: var(--danger-bg); }
    .req-row.gap .ev-mark { color: var(--danger); }
    .req-row.gap .ev-name { color: var(--muted); }

    .cov-act { display: inline-flex; align-items: center; gap: 5px; align-self: flex-start; margin-top: 8px; padding: 5px 10px; border-radius: 7px; border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 12px; font-weight: 500; color: var(--fg); text-decoration: none; cursor: pointer; transition: border-color 0.12s, color 0.12s, background 0.12s; }
    .cov-act:hover { border-color: var(--accent); color: var(--accent); }
    .cov-act svg { width: 12px; height: 12px; }
    .req-row.gap .cov-act { border-color: color-mix(in oklch, var(--danger) 35%, var(--border)); color: var(--danger); }
    .req-row.gap .cov-act:hover { background: var(--danger-bg); border-color: var(--danger); }

    .cm-foot { display: flex; gap: 18px; padding: 11px 20px; border-top: 1px solid var(--border); font-family: var(--font-mono); font-size: 12px; color: var(--muted); background: var(--surface-2); flex-wrap: wrap; }
    .cov-empty { padding: 26px 20px; text-align: center; color: var(--faint); font-size: 13px; }

    .anim-fade { animation: anim-fade 0.3s ease both; }
    @keyframes anim-fade { from { opacity: 0; } to { opacity: 1; } }

    @media (max-width: 1080px) {
      .cm-head, .req-row { grid-template-columns: 1fr; }
      .rq-left { border-right: 0; border-bottom: 1px solid color-mix(in oklch, var(--border) 70%, transparent); }
      .cm-head > div:first-child { border-right: 0; }
    }
  `,
})
export class CoverageMap {
  readonly jd = input<JobRequirements | null>(null);
  readonly gap = input<GapReport | null>(null);

  readonly act = output<{ name: string; intent: 'matched' | 'semantic' | 'gap' }>();

  protected readonly covFilter = signal<CovFilter>('all');

  protected readonly rows = computed<ReqRow[]>(() => {
    const jd = this.jd();
    const gap = this.gap();
    return jd && gap ? buildRows(jd, gap) : [];
  });

  protected readonly counts = computed(() => {
    const rows = this.rows();
    return {
      all: rows.length,
      exact: rows.filter((r) => r.state === 'exact').length,
      semantic: rows.filter((r) => r.state === 'semantic').length,
      gap: rows.filter((r) => r.state === 'gap').length,
    };
  });

  protected readonly visibleRows = computed(() => {
    const f = this.covFilter();
    const rows = this.rows();
    if (f === 'all') return rows;
    return rows.filter((r) => r.state === f);
  });

  protected readonly emptyFilterNote = computed(() =>
    this.covFilter() === 'gap'
      ? 'No gaps — every requirement is covered.'
      : 'Nothing matches this filter.',
  );

  protected setFilter(f: CovFilter): void {
    this.covFilter.set(f);
  }
}

/** Cross-reference every JD requirement against the gap report's matches. Solid
 *  matches (`matched`) are exact (green) or semantic (amber); `weak` matches are
 *  amber too; anything unmatched is a gap (red). Iteration order follows the JD
 *  (required first, then preferred) so the table reads like the posting. */
function buildRows(jd: JobRequirements, gap: GapReport): ReqRow[] {
  const matched = new Map<string, GapMatch>();
  for (const m of gap.matched ?? []) matched.set(m.jd_skill.name, m);
  const weak = new Map<string, GapMatch>();
  for (const w of gap.weak ?? []) weak.set(w.jd_skill.name, w);

  const skills = [...(jd.required_skills ?? []), ...(jd.preferred_skills ?? [])];
  return skills.map((s) => {
    const m = matched.get(s.name);
    const w = weak.get(s.name);
    const base = {
      name: s.name,
      importance: s.importance,
      category: s.category,
      context: s.context_phrases?.[0] ?? '',
    };
    if (m && !m.semantic) {
      return { ...base, state: 'exact' as const, mark: '✓', tag: 'exact match', evidence: m.dataset_name, action: 'Refine', intent: 'matched' as const };
    }
    if (m) {
      return { ...base, state: 'semantic' as const, mark: '≈', tag: 'semantic match', evidence: m.dataset_name, action: 'Strengthen', intent: 'semantic' as const };
    }
    if (w) {
      return { ...base, state: 'semantic' as const, mark: '≈', tag: 'weak match', evidence: w.dataset_name, action: 'Strengthen', intent: 'semantic' as const };
    }
    return { ...base, state: 'gap' as const, mark: '✕', tag: `gap · ${s.category}`, evidence: 'No matching experience', action: 'Fill the gap', intent: 'gap' as const };
  });
}
