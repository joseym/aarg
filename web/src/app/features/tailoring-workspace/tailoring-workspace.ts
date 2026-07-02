import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
} from '@angular/core';
import { RouterLink } from '@angular/router';
import { HttpErrorResponse } from '@angular/common/http';

import { ApiService } from '../../services/api.service';
import { WasmService } from '../../services/wasm.service';
import type {
  AdversarialReport,
  BuildDetail,
  GapReport,
  JobRequirements,
  ProvenanceReport,
  ResumeDataset,
  VariantPayload,
} from '../../models';

import { ResumePreview } from './resume-preview';
import { CoverageMap } from './coverage-map';
import { ReviewerRail } from './reviewer-rail';
import { RefineDrawer } from './refine-drawer';
import {
  band,
  buildCoverageRows,
  buildObjectionVMs,
  buildPreviewModel,
  objectionId,
  pct,
  provenanceIndex,
  targetKey,
  type CoverageRow,
  type ObjectionVM,
  type PreviewModel,
} from './workspace.model';

/** `BuildDetail` (models/build.ts) does not yet type a human `VariantPayload`;
 *  the serve bundle carries one on disk. Read it defensively and flag the gap
 *  rather than editing the shared model. */
interface BuildBundle extends BuildDetail {
  human_payload?: VariantPayload;
}

type ClaimState = 'ok' | 'checking' | 'flag';

/** The tailoring workspace (`/build/:id/tailor`): a two-pane screen — the
 *  editable, provenance-aware résumé preview on the left; the reviewer's verdict
 *  and objection triage on the right. This wave wires read + deterministic
 *  (wasm) paths; the LLM copilots are stubbed behind {@link RefineDrawer}. */
@Component({
  selector: 'app-tailoring-workspace',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, ResumePreview, CoverageMap, ReviewerRail, RefineDrawer],
  template: `
    <!-- ── workspace context bar (job + coverage + actions) ── -->
    <div class="ctxbar">
      <a class="btn btn-ghost" routerLink="/">← Back to builds</a>
      <div class="ctx">
        <div class="ctx-job">{{ jobTitle() }}</div>
        <div class="ctx-co">{{ jobCompany() }} · tailoring workspace</div>
      </div>
      @if (coverage(); as cov) {
        <span class="score-chip" [attr.data-band]="cov.band">
          <span class="dot"></span>
          <span class="num">{{ cov.pct }}%</span> · {{ cov.matched }}/{{ cov.total }} matched
        </span>
      }
      <div class="spacer"></div>
      <button class="btn" type="button" (click)="downloadPdf()" [disabled]="downloading()">
        {{ downloading() ? 'Rendering…' : 'Download PDF' }}
      </button>
      <a class="btn btn-primary" routerLink="/">New Build</a>
    </div>

    @if (loading()) {
      <div class="panel muted">Loading build {{ id() }}…</div>
    } @else if (error()) {
      <div class="panel">
        <b>Couldn’t load build {{ id() }}.</b>
        <p class="muted">{{ error() }}</p>
      </div>
    } @else {
      <div class="work">
        <!-- ── left: preview / coverage ── -->
        <section class="col-preview">
          <div class="pv-head">
            <div class="segmented" role="tablist" aria-label="Preview mode">
              <button role="tab" [attr.aria-selected]="view() === 'preview'" [class.on]="view() === 'preview'" (click)="view.set('preview')">Final preview</button>
              <button role="tab" [attr.aria-selected]="view() === 'coverage'" [class.on]="view() === 'coverage'" (click)="view.set('coverage')">Coverage map</button>
            </div>

            <div class="cc-wrap">
              <button class="claimcheck" [attr.data-state]="claimState()" (click)="runClaimCheck()" [attr.aria-busy]="claimState() === 'checking'">
                <span class="cc-dot" aria-hidden="true"></span>
                <span>{{ claimText() }}</span>
              </button>
              <button
                class="cc-info"
                type="button"
                aria-label="What the claim check verifies"
                (mouseenter)="infoOpen.set(true)"
                (focus)="infoOpen.set(true)"
                (mouseleave)="infoOpen.set(false)"
                (blur)="infoOpen.set(false)"
              >i</button>
              @if (infoOpen()) {
                <div class="cc-tip" role="tooltip">
                  Each line is checked against your dataset:
                  <b>verbatim</b> — copied from your evidence;
                  <b>grounded</b> — closest recorded match;
                  <b>your own edit</b> — you changed it. A flag means a line isn’t
                  yet traced and needs your confirmation.
                </div>
              }
            </div>
          </div>

          @if (view() === 'preview') {
            @if (previewModel(); as m) {
              <app-resume-preview [model]="m" (edit)="onEdit($event)" />
              <div class="fidelity">
                <span class="tag">HTML · editing</span>
                <span>Live in-browser preview — edit any line. Facts stay identical to the</span>
                <button class="btn btn-sm" type="button" (click)="downloadPdf()">pixel-perfect PDF ↓</button>
              </div>
            } @else {
              <div class="panel muted">This build has no résumé payload to preview.</div>
            }
          } @else {
            <app-coverage-map [rows]="coverageRows()" />
          }
        </section>

        <!-- ── right: reviewer verdict + objections ── -->
        <section class="col-work">
          @if (report(); as rep) {
            <app-reviewer-rail
              [report]="rep"
              [objections]="objectionVMs()"
              [accepted]="accepted()"
              [left]="leftIds()"
              [busy]="busy()"
              (refine)="onRefine($event)"
              (accept)="onAccept($event)"
              (leave)="onLeave($event)"
              (reopen)="onReopen($event)"
            />
          } @else {
            <div class="panel muted">No reviewer report on this build.</div>
          }
        </section>
      </div>
    }

    <app-refine-drawer [objection]="drawer()" (close)="drawer.set(null)" />

    @if (toast(); as t) {
      <div class="toast on" role="status">{{ t }}</div>
    }
  `,
  styles: `
    :host { display: block; }
    .ctxbar { display: flex; align-items: center; gap: 14px; margin-bottom: 20px; flex-wrap: wrap; }
    .ctx { display: flex; flex-direction: column; }
    .ctx-job { font-family: var(--font-display); font-size: 15px; }
    .ctx-co { color: var(--muted); font-size: 13px; }
    .spacer { flex: 1; }
    .score-chip { display: inline-flex; align-items: center; gap: 7px; padding: 4px 10px; border-radius: 999px; border: 1px solid var(--border); font-family: var(--font-mono); font-size: 12px; font-variant-numeric: tabular-nums; }
    .score-chip .dot { width: 7px; height: 7px; border-radius: 2px; background: var(--faint); }
    .score-chip[data-band='ok'] { color: var(--success); border-color: color-mix(in oklch, var(--success) 40%, var(--border)); }
    .score-chip[data-band='ok'] .dot { background: var(--success); }
    .score-chip[data-band='warn'] { color: var(--warn); border-color: color-mix(in oklch, var(--warn) 40%, var(--border)); }
    .score-chip[data-band='warn'] .dot { background: var(--warn); }
    .score-chip[data-band='bad'] { color: var(--danger); border-color: color-mix(in oklch, var(--danger) 40%, var(--border)); }
    .score-chip[data-band='bad'] .dot { background: var(--danger); }

    .btn { display: inline-flex; align-items: center; gap: 8px; height: 34px; padding: 0 14px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 14px; font-weight: 500; color: inherit; cursor: pointer; text-decoration: none; }
    .btn:hover:not(:disabled) { border-color: var(--fg); }
    .btn:disabled { opacity: 0.6; cursor: default; }
    .btn-primary { background: var(--accent); color: oklch(97% 0.02 40); border-color: var(--accent); }
    .btn-primary:hover { background: var(--accent-2); border-color: var(--accent-2); }
    .btn-ghost { border-color: transparent; background: transparent; }
    .btn-ghost:hover { border-color: var(--border); }
    .btn-sm { height: 28px; padding: 0 10px; font-size: 13px; }
    .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }

    .work { display: grid; grid-template-columns: minmax(0, 1fr) 440px; gap: 0; align-items: start; }
    .col-preview { padding: 4px 28px 40px 0; border-right: 1px solid var(--border); }
    .col-work { padding: 4px 0 40px 26px; position: sticky; top: 76px; }

    .pv-head { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
    .segmented { display: inline-flex; padding: 3px; gap: 3px; background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px; }
    .segmented button { padding: 6px 14px; border: 0; border-radius: 999px; background: transparent; font-size: 13px; font-weight: 500; color: var(--muted); cursor: pointer; }
    .segmented button.on { background: var(--surface); color: var(--fg); box-shadow: 0 1px 2px color-mix(in oklch, var(--fg) 12%, transparent); }
    .segmented button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }

    .cc-wrap { position: relative; display: inline-flex; align-items: center; gap: 6px; }
    .claimcheck { display: inline-flex; align-items: center; gap: 9px; padding: 7px 13px; border-radius: 999px; cursor: pointer; font-family: var(--font-mono); font-size: 12px; border: 1px solid; background: var(--surface-2); color: var(--muted); }
    .claimcheck .cc-dot { width: 9px; height: 9px; border-radius: 50%; background: var(--muted); }
    .claimcheck[data-state='ok'] { color: var(--success); border-color: color-mix(in oklch, var(--success) 38%, var(--border)); background: var(--success-bg); }
    .claimcheck[data-state='ok'] .cc-dot { background: var(--success); }
    .claimcheck[data-state='checking'] { color: var(--muted); border-color: var(--border); }
    .claimcheck[data-state='flag'] { color: var(--warn); border-color: color-mix(in oklch, var(--warn) 45%, var(--border)); background: var(--warn-bg); }
    .claimcheck[data-state='flag'] .cc-dot { background: var(--warn); }
    .cc-info { width: 20px; height: 20px; border-radius: 50%; border: 1px solid var(--border); background: var(--surface); color: var(--muted); font-family: var(--font-mono); font-size: 11px; line-height: 1; cursor: help; }
    .cc-info:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .cc-tip { position: absolute; top: 30px; right: 0; z-index: 20; width: 300px; background: var(--fg); color: oklch(96% 0.01 80); padding: 12px 14px; border-radius: 9px; font-size: 12.5px; line-height: 1.5; box-shadow: 0 12px 30px -12px color-mix(in oklch, var(--fg) 60%, transparent); }
    .cc-tip b { color: oklch(99% 0.01 80); }

    .fidelity { display: flex; align-items: center; gap: 10px; margin-top: 14px; font-size: 12.5px; color: var(--muted); flex-wrap: wrap; }
    .fidelity .tag { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--faint); border: 1px solid var(--border); border-radius: 999px; padding: 3px 8px; }

    .panel { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 22px; max-width: 62ch; display: flex; flex-direction: column; gap: 10px; align-items: flex-start; }
    .muted { color: var(--muted); }

    .toast { position: fixed; bottom: 24px; left: 50%; transform: translateX(-50%); z-index: 80; background: var(--fg); color: oklch(97% 0.01 80); padding: 12px 18px; border-radius: 11px; font-size: 13.5px; box-shadow: 0 16px 40px -16px color-mix(in oklch, var(--fg) 60%, transparent); }

    @media (max-width: 1080px) {
      .work { grid-template-columns: 1fr; }
      .col-preview { border-right: 0; border-bottom: 1px solid var(--border); padding: 4px 0 32px; }
      .col-work { position: static; padding: 26px 0 40px; }
    }
    @media (max-width: 720px) {
      .ctxbar { gap: 10px; }
      .spacer { display: none; }
    }
  `,
})
export class TailoringWorkspace {
  private readonly api = inject(ApiService);
  private readonly wasm = inject(WasmService);

  readonly id = input.required<string>();

  // ── raw loaded data ──────────────────────────────────────────────────
  private readonly bundle = signal<BuildBundle | null>(null);
  private readonly dataset = signal<ResumeDataset | null>(null);
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);

  // ── deterministic (wasm) results ─────────────────────────────────────
  private readonly provReport = signal<ProvenanceReport | null>(null);
  private readonly coverageReport = signal<{ pct: number; band: string; matched: number; total: number } | null>(null);
  private readonly claimsFlagged = signal(false);

  // ── local UI state ───────────────────────────────────────────────────
  protected readonly view = signal<'preview' | 'coverage'>('preview');
  protected readonly infoOpen = signal(false);
  protected readonly edits = signal<Record<string, string>>({});
  protected readonly accepted = signal<ReadonlySet<string>>(new Set());
  protected readonly leftIds = signal<ReadonlySet<string>>(new Set());
  protected readonly busy = signal<string | null>(null);
  protected readonly drawer = signal<ObjectionVM | null>(null);
  protected readonly downloading = signal(false);
  private readonly claimChecking = signal(false);
  private readonly _toast = signal<string | null>(null);
  protected readonly toast = this._toast.asReadonly();

  // ── derived view state ───────────────────────────────────────────────
  private readonly jd = computed<JobRequirements | null>(() => this.bundle()?.jd ?? null);
  protected readonly report = computed<AdversarialReport | null>(() => this.bundle()?.adversarial_report ?? null);
  protected readonly coverage = computed(() => this.coverageReport());

  protected readonly jobTitle = computed(
    () => this.jd()?.title ?? this.bundle()?.canonical?.target_title ?? 'Untitled build',
  );
  protected readonly jobCompany = computed(() => this.jd()?.company ?? '');

  // The preview renders a VariantPayload. Prefer the human variant, but most
  // builds are rendered ATS-only (no human_payload.json), so fall back to the
  // ATS payload — it's the same shape, so the preview builder works on either.
  // Only when neither exists is there nothing to show.
  private readonly previewPayload = computed<VariantPayload | null>(
    () => this.bundle()?.human_payload ?? this.bundle()?.ats_payload ?? null,
  );

  protected readonly previewModel = computed<PreviewModel | null>(() => {
    const payload = this.previewPayload();
    if (!payload) return null;
    // Guard the whole screen: this computed feeds the template, so a throw here
    // (a payload/provenance shape the builder doesn't expect) would abort the
    // render pass and blank the reviewer rail too. Degrade to the empty preview
    // state and log, rather than take the page down.
    try {
      return buildPreviewModel(payload, provenanceIndex(this.provReport()), this.edits(), this.dataset());
    } catch (e) {
      console.error('failed to build the résumé preview', e);
      return null;
    }
  });

  protected readonly coverageRows = computed<CoverageRow[]>(() =>
    buildCoverageRows(this.bundle()?.gap_report ?? null),
  );

  protected readonly objectionVMs = computed<ObjectionVM[]>(() =>
    buildObjectionVMs(this.report()?.objections ?? [], this.previewPayload(), this.dataset()),
  );

  protected readonly claimState = computed<ClaimState>(() => {
    if (this.claimChecking()) return 'checking';
    const edits = this.edits();
    const unrecorded = (this.provReport()?.lines ?? []).some(
      (l) => l.status === 'unrecorded' && !Object.prototype.hasOwnProperty.call(edits, keyOf(l)),
    );
    return unrecorded || this.claimsFlagged() ? 'flag' : 'ok';
  });

  private readonly flaggedCount = computed(() => {
    const edits = this.edits();
    return (this.provReport()?.lines ?? []).filter(
      (l) => l.status === 'unrecorded' && !Object.prototype.hasOwnProperty.call(edits, keyOf(l)),
    ).length;
  });

  protected readonly claimText = computed(() => {
    switch (this.claimState()) {
      case 'checking':
        return 'Checking every line against your evidence…';
      case 'flag': {
        const n = this.flaggedCount();
        return `${n || 1} line${n === 1 ? '' : 's'} need your confirmation before it lands`;
      }
      default:
        return 'Every line traces to your evidence';
    }
  });

  constructor() {
    // Load whenever the route id changes; then run the deterministic checks.
    effect(() => {
      const id = this.id();
      this.reset();
      this.api.getBuild(id).subscribe({
        next: (d) => {
          this.bundle.set(d as BuildBundle);
          this.loadDataset();
        },
        error: (err: unknown) => {
          this.error.set(errMessage(err));
          this.loading.set(false);
        },
      });
    });
  }

  private reset(): void {
    this.loading.set(true);
    this.error.set(null);
    this.bundle.set(null);
    this.dataset.set(null);
    this.provReport.set(null);
    this.coverageReport.set(null);
    this.claimsFlagged.set(false);
    this.edits.set({});
    this.accepted.set(new Set());
    this.leftIds.set(new Set());
    this.drawer.set(null);
    this.view.set('preview');
  }

  private loadDataset(): void {
    this.api.getDataset().subscribe({
      next: (ds) => {
        this.dataset.set(ds);
        this.seedAccepted(ds);
        this.loading.set(false);
        void this.recompute();
      },
      error: () => {
        // The preview still renders without a dataset; provenance just resolves
        // to bare ids. Don't hard-fail the whole screen.
        this.loading.set(false);
        void this.recompute();
      },
    });
  }

  /** Pre-mark objections the dataset already records as dismissed. */
  private seedAccepted(ds: ResumeDataset): void {
    const dismissed = dismissedList(ds);
    if (dismissed.length === 0) return;
    const keys = new Set(dismissed.map((d) => `${targetKey(d.target)}::${d.kind}`));
    const ids = new Set<string>();
    for (const o of this.report()?.objections ?? []) {
      if (keys.has(objectionId(o))) ids.add(objectionId(o));
    }
    this.accepted.set(ids);
  }

  /** Run the pure deterministic checks: provenance, weighted coverage, claims. */
  private async recompute(): Promise<void> {
    const b = this.bundle();
    const ds = this.dataset();
    const canonical = b?.canonical;

    if (canonical && ds) {
      try {
        this.provReport.set(await this.wasm.checkProvenance(canonical, ds));
      } catch {
        this.provReport.set(null);
      }
    }

    const payload = this.previewPayload();
    if (canonical && payload) {
      try {
        const claims = (await this.wasm.checkClaims(canonical, payload)) as {
          divergences?: unknown[];
          ok?: boolean;
        };
        this.claimsFlagged.set((claims.divergences?.length ?? 0) > 0 || claims.ok === false);
      } catch {
        this.claimsFlagged.set(false);
      }
    }

    const gap = b?.gap_report ?? null;
    const jd = this.jd();
    if (gap && jd) {
      try {
        const wc = await this.wasm.weightedCoverage(gap, jd);
        this.coverageReport.set(coverageChip(gap, wc.score));
      } catch {
        this.coverageReport.set(coverageChip(gap, b?.ats_report?.coverage ?? 0));
      }
    } else if (gap) {
      this.coverageReport.set(coverageChip(gap, b?.ats_report?.coverage ?? 0));
    }
  }

  // ── free edit ────────────────────────────────────────────────────────
  protected onEdit(e: { key: string; text: string }): void {
    this.edits.update((m) => ({ ...m, [e.key]: e.text }));
    // Per the UI decision: re-run the deterministic provenance check on edit.
    // The edited line is shown as "your own edit" locally; a later wave will
    // rebuild an edited canonical so the core can re-check the changed line too.
    void this.recompute();
    this.showToast('Edit kept locally — not yet saved to your dataset.');
  }

  // ── claim check ──────────────────────────────────────────────────────
  protected runClaimCheck(): void {
    this.claimChecking.set(true);
    void this.recompute().finally(() => this.claimChecking.set(false));
  }

  // ── objection triage ─────────────────────────────────────────────────
  protected onLeave(o: ObjectionVM): void {
    this.leftIds.update((s) => withAdded(s, o.id));
    this.accepted.update((s) => withRemoved(s, o.id));
  }

  protected onReopen(o: ObjectionVM): void {
    this.leftIds.update((s) => withRemoved(s, o.id));
    this.accepted.update((s) => withRemoved(s, o.id));
  }

  protected onRefine(o: ObjectionVM): void {
    this.drawer.set(o);
  }

  /** Accept as intentional → append a `DismissedObjection` to the dataset and
   *  PUT it (validate-gated). A 422 surfaces the validation findings. */
  protected onAccept(o: ObjectionVM): void {
    const ds = this.dataset();
    if (!ds) {
      this.showToast('Dataset unavailable — cannot persist this dismissal.');
      return;
    }
    const next = withDismissal(ds, { target: o.objection.target, kind: o.objection.kind });
    this.busy.set(o.id);
    this.api.putDataset(next).subscribe({
      next: (saved) => {
        this.dataset.set(saved);
        this.accepted.update((s) => withAdded(s, o.id));
        this.leftIds.update((s) => withRemoved(s, o.id));
        this.busy.set(null);
        this.showToast('Accepted — the reviewer won’t raise this again.');
      },
      error: (err: unknown) => {
        this.busy.set(null);
        const msg =
          err instanceof HttpErrorResponse && err.status === 422
            ? `Validation blocked the save: ${findings(err)}`
            : errMessage(err);
        this.showToast(msg);
      },
    });
  }

  // ── download ─────────────────────────────────────────────────────────
  protected downloadPdf(): void {
    const b = this.bundle();
    if (!b) return;
    const payload = this.previewPayload();
    this.downloading.set(true);
    const done = () => this.downloading.set(false);
    if (payload) {
      // Render the variant actually being previewed (human when present, else
      // the ATS payload), not a hardcoded "human" — otherwise an ATS-only build
      // would ask the server to render ATS content with the human template.
      this.api.render(payload.variant, payload, b.meta?.template).subscribe({
        next: (blob) => {
          triggerDownload(blob, `${this.id()}-${payload.variant}.pdf`);
          done();
        },
        error: (err: unknown) => {
          done();
          this.showToast(`Render failed: ${errMessage(err)}`);
        },
      });
    } else if (b.pdfs.length > 0) {
      this.api.getBuildFile(this.id(), b.pdfs[0]).subscribe({
        next: (blob) => {
          triggerDownload(blob, b.pdfs[0]);
          done();
        },
        error: (err: unknown) => {
          done();
          this.showToast(`Download failed: ${errMessage(err)}`);
        },
      });
    } else {
      done();
      this.showToast('No rendered PDF or payload available for this build.');
    }
  }

  private showToast(msg: string): void {
    this._toast.set(msg);
    setTimeout(() => this._toast.set(null), 3200);
  }
}

// ── module-local helpers ───────────────────────────────────────────────

interface DismissedObjection {
  target: ObjectionVM['objection']['target'];
  kind: ObjectionVM['objection']['kind'];
}

function keyOf(l: ProvenanceReport['lines'][number]): string {
  const loc = l.location;
  switch (loc.kind) {
    case 'summary':
      return 'summary';
    case 'role_bullet':
      return `bullet:${loc.role_id}:${loc.bullet_index}`;
    case 'skill':
      return `skill:${loc.index}`;
  }
}

function coverageChip(gap: GapReport, coverage01: number): { pct: number; band: string; matched: number; total: number } {
  const matched = gap.matched.length;
  const total = matched + gap.weak.length + gap.unknown.length;
  return { pct: pct(coverage01), band: band(coverage01), matched, total };
}

function dismissedList(ds: ResumeDataset): DismissedObjection[] {
  const raw = ds.metadata?.['dismissed_objections'];
  return Array.isArray(raw) ? (raw as DismissedObjection[]) : [];
}

function withDismissal(ds: ResumeDataset, d: DismissedObjection): ResumeDataset {
  const list = dismissedList(ds);
  const exists = list.some((x) => targetKey(x.target) === targetKey(d.target) && x.kind === d.kind);
  const dismissed_objections = exists ? list : [...list, d];
  return { ...ds, metadata: { ...ds.metadata, dismissed_objections } };
}

function withAdded(s: ReadonlySet<string>, id: string): ReadonlySet<string> {
  const next = new Set(s);
  next.add(id);
  return next;
}
function withRemoved(s: ReadonlySet<string>, id: string): ReadonlySet<string> {
  const next = new Set(s);
  next.delete(id);
  return next;
}

function triggerDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

function errMessage(err: unknown): string {
  if (err instanceof HttpErrorResponse) return err.message;
  if (err instanceof Error) return err.message;
  return 'request failed';
}

function findings(err: HttpErrorResponse): string {
  const body = err.error;
  if (typeof body === 'string') return body;
  if (body && typeof body === 'object') {
    const f = (body as { findings?: unknown; message?: unknown }).findings ?? (body as { message?: unknown }).message;
    if (Array.isArray(f)) return f.map(String).join('; ');
    if (typeof f === 'string') return f;
  }
  return 'the edited dataset failed validation';
}
