import { ChangeDetectionStrategy, Component, computed, input, output, signal } from '@angular/core';

import type { AdversarialReport } from '../../models';
import type { ObjectionVM, TriageStatus } from './workspace.model';
import { band, pct } from './workspace.model';
import { normalizeDashes } from '../../shared/normalize-dashes';

type StatusFilter = 'open' | 'accepted' | 'refined' | 'left';
type TypeFilter = ObjectionVM['type'];

/** The right rail: the reviewer's verdict (persona notes + the overall score as
 *  a rail number, not the headline) and the objection cards with multi-select
 *  status/type facets. Triage actions bubble to the container: Refine it opens a
 *  runs the copilot, Accept persists a dismissal, Leave it is session-local. */
@Component({
  selector: 'app-reviewer-rail',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="verdict" [style.borderLeftColor]="'var(--' + verdictToken() + ')'">
      <div class="vhead">
        <div class="vtitle">Reviewer verdict</div>
        <div class="vscore">
          <span class="n" [attr.data-band]="verdictBand()">{{ scorePct() }}</span><span class="pct">%</span>
        </div>
      </div>
      <div class="rule"></div>
      <p>{{ personaNotes() || 'No reviewer notes on this build.' }}</p>
    </div>

    <div class="sec-h">
      <h3>Objections</h3>
      <span class="n">{{ openCount() }} open · {{ acceptedCount() }} accepted</span>
    </div>

    <!-- facets: OR within a facet, AND across facets -->
    <div class="obj-filter">
      <div class="of-group">
        <span class="of-label">Status</span>
        <button class="obj-chip" [class.on]="statusSel().size === 0" (click)="clearStatus()">All</button>
        @for (s of statusFacets(); track s.value) {
          <button class="obj-chip" [class.on]="statusSel().has(s.value)" (click)="toggleStatus(s.value)">
            {{ s.label }} <span class="oc-n">{{ s.count }}</span>
          </button>
        }
      </div>
      <div class="of-group">
        <span class="of-label">Type</span>
        <button class="obj-chip" [class.on]="typeSel().size === 0" (click)="clearType()">All</button>
        @for (t of typeFacets(); track t.value) {
          <button class="obj-chip" [attr.data-t]="t.value" [class.on]="typeSel().has(t.value)" (click)="toggleType(t.value)">
            <span class="oc-dot"></span>{{ t.label }} <span class="oc-n">{{ t.count }}</span>
          </button>
        }
      </div>
    </div>

    @if (visible().length === 0) {
      <div class="obj-empty">No objections match these filters.</div>
    } @else {
      @for (o of visible(); track o.id; let i = $index) {
        <article class="card anim-up" [class.resolved]="statusOf(o.id) !== 'open'" [style.animationDelay.ms]="i * 36">
          @if (statusOf(o.id) === 'open') {
            <div class="c-top">
              <span class="c-type" [attr.data-t]="o.type">{{ o.typeLabel }}</span>
              <span class="c-loc">{{ o.targetLabel }} · {{ o.severity }}</span>
            </div>
            @if (o.flaggedText) {
              <div class="c-line">{{ o.flaggedText }}</div>
            }
            <div class="c-reason">{{ o.message }}</div>
            @if (o.suggestion) {
              <div class="c-sug"><span class="pl">Suggestion · </span>{{ o.suggestion }}</div>
            }
            <div class="c-actions">
              <button class="btn btn-primary btn-sm" (click)="refine.emit(o)">Refine it</button>
              <button class="btn btn-sm" (click)="accept.emit(o)" [disabled]="busy() === o.id">
                {{ busy() === o.id ? 'Saving…' : 'Accept as intentional' }}
              </button>
              <button class="btn btn-ghost btn-sm" (click)="leave.emit(o)">Leave it</button>
            </div>
          } @else if (statusOf(o.id) === 'refined') {
            <div class="c-resolved refined">
              <span class="rmark">✓</span>
              <span>Refined: evidence recorded · reflects on your next build</span>
            </div>
          } @else {
            <div class="c-resolved" [class.left]="statusOf(o.id) === 'left'">
              <span class="rmark">{{ statusOf(o.id) === 'left' ? '·' : '✓' }}</span>
              <span>{{ statusOf(o.id) === 'left' ? 'Left for now' : 'Accepted as intentional · won’t re-flag' }}</span>
              <button class="undo" (click)="reopen.emit(o)">undo</button>
            </div>
          }
        </article>
      }
    }
  `,
  styles: `
    :host { display: block; }
    .verdict { border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--surface); padding: 18px 18px 16px; border-left: 3px solid var(--fg); }
    .vhead { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 9px; }
    .vtitle { font-size: 15px; font-family: var(--font-display); }
    .vscore { display: flex; align-items: baseline; gap: 4px; }
    .vscore .n { font-family: var(--font-display); font-size: 30px; line-height: 1; font-weight: 600; }
    .vscore .n[data-band='ok'] { color: var(--success); }
    .vscore .n[data-band='warn'] { color: var(--warn); }
    .vscore .n[data-band='bad'] { color: var(--danger); }
    .vscore .pct { font-family: var(--font-mono); font-size: 13px; color: var(--muted); }
    .verdict p { font-size: 13.5px; color: var(--muted); line-height: 1.5; margin: 0; }
    .verdict .rule { height: 3px; border-radius: 2px; margin: 12px 0; background: linear-gradient(90deg, var(--accent), var(--accent-2)); width: 55%; }

    .sec-h { display: flex; align-items: center; justify-content: space-between; margin: 26px 0 12px; }
    .sec-h h3 { font-size: 16px; }
    .sec-h .n { font-family: var(--font-mono); font-size: 11px; color: var(--faint); }

    .obj-filter { display: flex; flex-direction: column; gap: 9px; margin-bottom: 14px; }
    .of-group { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
    .of-label { font-family: var(--font-mono); font-size: 9.5px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--faint); width: 46px; flex-shrink: 0; }
    .obj-chip { display: inline-flex; align-items: center; gap: 6px; padding: 4px 10px; border-radius: 999px; border: 1px solid var(--border); background: var(--surface); font-size: 12px; color: var(--muted); cursor: pointer; }
    .obj-chip:hover { border-color: var(--fg); color: var(--fg); }
    .obj-chip:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .obj-chip .oc-dot { width: 8px; height: 8px; border-radius: 2px; background: var(--faint); }
    .obj-chip[data-t='unsupported'] .oc-dot { background: var(--t-unsupported); }
    .obj-chip[data-t='metric'] .oc-dot { background: var(--t-metric); }
    .obj-chip[data-t='weak'] .oc-dot { background: var(--t-weak); }
    .obj-chip[data-t='skills'] .oc-dot { background: var(--t-skills); }
    .obj-chip[data-t='layout'] .oc-dot { background: var(--t-layout); }
    .obj-chip[data-t='overall'] .oc-dot { background: var(--t-overall); }
    .obj-chip .oc-n { font-family: var(--font-mono); font-variant-numeric: tabular-nums; font-size: 10.5px; color: var(--faint); }
    .obj-chip.on { border-color: var(--accent); color: var(--accent); background: var(--accent-soft); }
    .obj-chip.on .oc-n { color: var(--accent); }
    .obj-empty { padding: 22px 14px; text-align: center; color: var(--faint); font-size: 13px; border: 1px dashed var(--border); border-radius: var(--radius-lg); }

    .card { border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--surface); margin-bottom: 12px; overflow: hidden; }
    .card.resolved { background: var(--surface-2); }
    .c-top { display: flex; align-items: center; gap: 9px; padding: 12px 15px 0; flex-wrap: wrap; }
    .c-type { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.09em; text-transform: uppercase; padding: 3px 8px; border-radius: 5px; border: 1px solid; }
    .c-type[data-t='unsupported'] { color: var(--t-unsupported); border-color: color-mix(in oklch, var(--t-unsupported) 40%, var(--border)); background: var(--danger-bg); }
    .c-type[data-t='metric'] { color: var(--t-metric); border-color: color-mix(in oklch, var(--t-metric) 40%, var(--border)); background: var(--accent-soft); }
    .c-type[data-t='weak'] { color: var(--t-weak); border-color: color-mix(in oklch, var(--t-weak) 45%, var(--border)); background: var(--warn-bg); }
    .c-type[data-t='skills'] { color: var(--t-skills); border-color: color-mix(in oklch, var(--t-skills) 40%, var(--border)); background: color-mix(in oklch, var(--t-skills) 8%, transparent); }
    .c-type[data-t='layout'] { color: var(--muted); border-color: var(--border); }
    .c-type[data-t='overall'] { color: var(--t-overall); border-color: var(--border); }
    .c-loc { font-family: var(--font-mono); font-size: 11px; color: var(--faint); text-transform: uppercase; }
    .c-line { margin: 10px 15px 0; padding: 10px 12px; border-left: 2px solid var(--border); background: var(--surface-2); border-radius: 0 6px 6px 0; font-family: var(--font-display); font-size: 14px; line-height: 1.4; }
    .c-reason { margin: 11px 15px 0; font-size: 13.5px; color: var(--muted); line-height: 1.5; }
    .c-sug { margin: 9px 15px 0; font-size: 13px; color: var(--fg); }
    .c-sug .pl { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.06em; text-transform: uppercase; color: var(--faint); }
    .c-actions { display: flex; gap: 8px; padding: 14px 15px; margin-top: 12px; border-top: 1px solid var(--border); flex-wrap: wrap; }
    .c-resolved { display: flex; align-items: center; gap: 9px; padding: 12px 15px; font-size: 13px; color: var(--muted); }
    .c-resolved .rmark { color: var(--success); font-family: var(--font-mono); }
    .c-resolved.left .rmark { color: var(--faint); }
    .undo { border: 0; background: none; color: var(--accent); font-size: 12px; text-decoration: underline; margin-left: auto; cursor: pointer; }

    .btn { display: inline-flex; align-items: center; gap: 6px; height: 30px; padding: 0 11px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); font-size: 13px; font-weight: 500; color: inherit; cursor: pointer; }
    .btn:hover { border-color: var(--fg); }
    .btn:disabled { opacity: 0.6; cursor: default; }
    .btn-primary { background: var(--accent); color: oklch(97% 0.02 40); border-color: var(--accent); }
    .btn-primary:hover { background: var(--accent-2); border-color: var(--accent-2); }
    .btn-ghost { border-color: transparent; background: transparent; }
    .btn-ghost:hover { border-color: var(--border); }
    .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  `,
  imports: [],
})
export class ReviewerRail {
  readonly report = input.required<AdversarialReport>();
  readonly objections = input.required<ObjectionVM[]>();
  /** Ids accepted (persisted), refined (evidence recorded this session), and
   *  left (session-only), owned by the container. */
  readonly accepted = input<ReadonlySet<string>>(new Set());
  readonly refined = input<ReadonlySet<string>>(new Set());
  readonly left = input<ReadonlySet<string>>(new Set());
  /** The id currently being persisted, to disable its Accept button. */
  readonly busy = input<string | null>(null);

  readonly refine = output<ObjectionVM>();
  readonly accept = output<ObjectionVM>();
  readonly leave = output<ObjectionVM>();
  readonly reopen = output<ObjectionVM>();

  protected readonly statusSel = signal<Set<StatusFilter>>(new Set());
  protected readonly typeSel = signal<Set<TypeFilter>>(new Set());

  /** MODEL text: the reviewer's free-text verdict reaches the view unscrubbed,
   *  so normalize dashes at this render boundary (see shared/normalize-dashes). */
  protected readonly personaNotes = computed(() => normalizeDashes(this.report().persona_notes));

  protected readonly scorePct = computed(() => pct(this.report().overall_score));
  protected readonly verdictBand = computed(() => band(this.report().overall_score));
  protected readonly verdictToken = computed(() => {
    const b = this.verdictBand();
    return b === 'ok' ? 'success' : b === 'warn' ? 'warn' : 'danger';
  });

  protected statusOf(id: string): TriageStatus {
    if (this.accepted().has(id)) return 'accepted';
    if (this.refined().has(id)) return 'refined';
    if (this.left().has(id)) return 'left';
    return 'open';
  }

  protected readonly openCount = computed(
    () => this.objections().filter((o) => this.statusOf(o.id) === 'open').length,
  );
  protected readonly acceptedCount = computed(() => this.accepted().size);

  protected readonly statusFacets = computed(() => {
    const objs = this.objections();
    return [
      { value: 'open' as const, label: 'Open', count: objs.filter((o) => this.statusOf(o.id) === 'open').length },
      { value: 'accepted' as const, label: 'Accepted', count: objs.filter((o) => this.statusOf(o.id) === 'accepted').length },
      { value: 'refined' as const, label: 'Refined', count: objs.filter((o) => this.statusOf(o.id) === 'refined').length },
      { value: 'left' as const, label: 'Left', count: objs.filter((o) => this.statusOf(o.id) === 'left').length },
    ];
  });

  protected readonly typeFacets = computed(() => {
    const counts = new Map<TypeFilter, number>();
    for (const o of this.objections()) counts.set(o.type, (counts.get(o.type) ?? 0) + 1);
    const order: { value: TypeFilter; label: string }[] = [
      { value: 'unsupported', label: 'Unsupported' },
      { value: 'metric', label: 'Metric' },
      { value: 'weak', label: 'Weak' },
      { value: 'skills', label: 'Skills' },
      { value: 'layout', label: 'Layout' },
      { value: 'overall', label: 'Overall' },
    ];
    return order.filter((t) => counts.has(t.value)).map((t) => ({ ...t, count: counts.get(t.value) ?? 0 }));
  });

  protected readonly visible = computed(() => {
    const st = this.statusSel();
    const ty = this.typeSel();
    return this.objections().filter(
      (o) => (st.size === 0 || st.has(this.statusOf(o.id))) && (ty.size === 0 || ty.has(o.type)),
    );
  });

  protected toggleStatus(v: StatusFilter): void {
    this.statusSel.update((s) => toggle(s, v));
  }
  protected toggleType(v: TypeFilter): void {
    this.typeSel.update((s) => toggle(s, v));
  }
  protected clearStatus(): void {
    this.statusSel.set(new Set());
  }
  protected clearType(): void {
    this.typeSel.set(new Set());
  }
}

function toggle<T>(set: Set<T>, v: T): Set<T> {
  const next = new Set(set);
  next.has(v) ? next.delete(v) : next.add(v);
  return next;
}
