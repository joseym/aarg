import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';

import { band } from '../features/tailoring-workspace/workspace.model';

/** One row of the score panel: a metric with its percent, band, meter fill, and
 *  a plain-language explanation. `emphasized` renders the headline metric
 *  (weighted coverage) slightly larger. */
interface ScoreRow {
  key: string;
  label: string;
  pct: number;
  band: 'ok' | 'warn' | 'bad';
  explain: string;
  subNote: string | null;
  emphasized: boolean;
}

/** One card that speaks a single score language: every number a build produces —
 *  weighted coverage, the reviewer's verdict, ATS keyword coverage, and the list
 *  (ranking) score — presented together, band-coloured and explained, so the
 *  three metrics that used to clash across the screen read as one system. Styled
 *  after the reviewer-verdict card (bordered surface, band left-rail, mono
 *  uppercase title, number + thin bar). Every input is optional; a row is skipped
 *  when its value is null. */
@Component({
  selector: 'app-score-panel',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="panel" [style.borderLeftColor]="'var(--' + headBandToken() + ')'">
      <div class="ptitle">Build scores</div>
      <div class="rows">
        @for (r of rows(); track r.key) {
          <div class="row" [class.emph]="r.emphasized">
            <div class="r-head">
              <span class="r-label">{{ r.label }}</span>
              <span class="r-pct" [attr.data-band]="r.band">{{ r.pct }}<span class="u">%</span></span>
            </div>
            <div class="r-meter"><i [attr.data-band]="r.band" [style.width.%]="r.pct"></i></div>
            <div class="r-explain">
              {{ r.explain }}
              @if (r.subNote) {
                <span class="r-sub">· {{ r.subNote }}</span>
              }
            </div>
          </div>
        }
      </div>
    </div>
  `,
  styles: `
    :host { display: block; }
    .panel {
      border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      background: var(--surface);
      border-left: 3px solid var(--fg);
      padding: 18px 20px 20px;
    }
    .ptitle {
      font-family: var(--font-mono);
      font-size: 10.5px;
      letter-spacing: 0.14em;
      text-transform: uppercase;
      color: var(--faint);
      margin-bottom: 16px;
    }
    .rows { display: flex; flex-direction: column; gap: 18px; }
    .row { display: flex; flex-direction: column; gap: 7px; }

    .r-head { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
    .r-label {
      font-family: var(--font-mono);
      font-size: 10.5px;
      letter-spacing: 0.1em;
      text-transform: uppercase;
      color: var(--muted);
    }
    .r-pct {
      font-family: var(--font-display);
      font-weight: 600;
      font-variant-numeric: tabular-nums;
      font-size: 22px;
      line-height: 1;
    }
    .r-pct .u { font-family: var(--font-mono); font-size: 0.5em; color: var(--muted); margin-left: 1px; }
    .r-pct[data-band='ok'] { color: var(--success); }
    .r-pct[data-band='warn'] { color: var(--warn); }
    .r-pct[data-band='bad'] { color: var(--danger); }

    /* The emphasized headline row (weighted coverage) reads a step larger. */
    .row.emph .r-pct { font-size: 34px; }
    .row.emph .r-label { color: var(--fg); }

    .r-meter { height: 4px; border-radius: 999px; background: var(--surface-2); overflow: hidden; }
    .r-meter i {
      display: block;
      height: 100%;
      border-radius: 999px;
      transition: width 0.6s cubic-bezier(0.2, 0.7, 0.2, 1);
    }
    .r-meter i[data-band='ok'] { background: var(--success); }
    .r-meter i[data-band='warn'] { background: var(--warn); }
    .r-meter i[data-band='bad'] { background: var(--danger); }

    .r-explain { font-size: 12.5px; color: var(--muted); line-height: 1.5; }
    .r-sub { color: var(--faint); }

    @media (prefers-reduced-motion: reduce) {
      .r-meter i { transition: none !important; }
    }
  `,
})
export class ScorePanel {
  /** All 0..1 fractions; a null input drops its row. */
  readonly weighted = input<number | null>(null);
  readonly verdict = input<number | null>(null);
  readonly ats = input<number | null>(null);
  /** For the weighted row's sub-note. */
  readonly matched = input<number>(0);
  readonly total = input<number>(0);

  /** The sidebar's ranking number: 60% reviewer verdict + 40% ATS keywords.
   *  This MUST mirror `src/history.rs` `fn combined`:
   *      0.6 * overall_score + 0.4 * coverage
   *  It's only defined when both inputs are present. */
  private readonly combined = computed<number | null>(() => {
    const v = this.verdict();
    const a = this.ats();
    if (v == null || a == null) return null;
    return 0.6 * v + 0.4 * a;
  });

  protected readonly rows = computed<ScoreRow[]>(() => {
    const out: ScoreRow[] = [];
    const w = this.weighted();
    if (w != null) {
      out.push({
        key: 'weighted',
        label: 'Weighted coverage',
        pct: toPct(w),
        band: band(w),
        explain:
          "How much of the job's requirements your evidence backs — critical requirements weigh 3× a nice-to-have.",
        subNote: `${this.matched()} of ${this.total()} matched`,
        emphasized: true,
      });
    }
    const v = this.verdict();
    if (v != null) {
      out.push({
        key: 'verdict',
        label: 'Reviewer verdict',
        pct: toPct(v),
        band: band(v),
        explain: 'A skeptical hiring-manager review of this draft — drives the revision loop.',
        subNote: null,
        emphasized: false,
      });
    }
    const a = this.ats();
    if (a != null) {
      out.push({
        key: 'ats',
        label: 'ATS keywords',
        pct: toPct(a),
        band: band(a),
        explain: "Job keywords actually found in the rendered PDF's text.",
        subNote: null,
        emphasized: false,
      });
    }
    const c = this.combined();
    if (c != null) {
      out.push({
        key: 'combined',
        label: 'List score',
        pct: toPct(c),
        band: band(c),
        explain: "The sidebar's ranking number: 60% reviewer verdict + 40% ATS keywords.",
        subNote: null,
        emphasized: false,
      });
    }
    return out;
  });

  /** Band token for the card's left rail — coloured by the headline row (the
   *  first present of weighted / verdict / ats), so the panel's accent tracks the
   *  metric a reader's eye lands on first. */
  protected readonly headBandToken = computed<string>(() => {
    const head = this.weighted() ?? this.verdict() ?? this.ats();
    if (head == null) return 'fg';
    const b = band(head);
    return b === 'ok' ? 'success' : b === 'warn' ? 'warn' : 'danger';
  });
}

function toPct(v: number): number {
  return Math.round(v * 100);
}
