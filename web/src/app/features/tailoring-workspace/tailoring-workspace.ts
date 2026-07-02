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
import { firstValueFrom } from 'rxjs';

import { ApiService } from '../../services/api.service';
import { WasmService } from '../../services/wasm.service';
import { CopilotHost } from '../../shared/copilot-host';
import { BuildRunner } from '../../services/build-runner';
import type {
  AdversarialReport,
  BuildDetail,
  Bullet,
  GapReport,
  JobRequirements,
  ProvenanceReport,
  ResumeDataset,
  VariantPayload,
} from '../../models';

import { ResumePreview } from './resume-preview';
import { ReviewerRail } from './reviewer-rail';
import { RefineDrawer } from './refine-drawer';
import { CoverageMap } from '../../shared/coverage-map';
import { ViewToggle } from '../../shared/view-toggle';
import { CoverageScore } from '../../shared/coverage-score';
import {
  band,
  buildObjectionVMs,
  buildPreviewModel,
  objectionId,
  pct,
  provenanceIndex,
  targetKey,
  type CopilotKind,
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
  imports: [RouterLink, ResumePreview, CoverageMap, ViewToggle, ReviewerRail, RefineDrawer, CoverageScore],
  template: `
    <!-- ── workspace context bar (job + coverage + actions) ── -->
    <div class="ctxbar">
      <div class="ctx">
        <div class="bh-kicker"><span class="dot"></span> Tailor job · complete</div>
        <div class="ctx-job">{{ jobTitle() }}</div>
        <div class="bh-meta">
          @if (jobCompany()) {
            <span class="co">{{ jobCompany() }}</span>
          }
          @if (locationLine()) {
            <span class="sep">·</span>
            <span>{{ locationLine() }}</span>
          }
          @if (sourceUrl(); as url) {
            <span class="sep">·</span>
            <a class="bh-src" [href]="url" rel="noopener">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                <path d="M10 13a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-1 1" />
                <path d="M14 11a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l1-1" />
              </svg>
              Source posting
            </a>
          } @else {
            <span class="sep">·</span>
            <span class="bh-src plain">
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                <path d="M4 3h11l5 5v13H4z" /><path d="M15 3v5h5" />
              </svg>
              Pasted description
            </span>
          }
        </div>
        <div class="ctx-status">
          <span class="status-pill" [attr.data-status]="status()"><span class="sp-dot"></span>{{ status() }}</span>
          @if (provenance(); as p) {
            <span class="bh-prov">Generated {{ p.created }} · {{ p.model }} · {{ p.tokens }} tokens · {{ p.template }} template</span>
          }
        </div>
      </div>
      <div class="spacer"></div>
      <button
        class="btn"
        type="button"
        title="Regenerate this résumé with your current evidence"
        (click)="retailor()"
        [disabled]="copilot.running()"
      >
        Retailor ↻
      </button>
      <button class="btn" type="button" (click)="downloadPdf()" [disabled]="downloading()">
        {{ downloading() ? 'Rendering…' : 'Download PDF' }}
      </button>
      <a class="btn btn-primary" routerLink="/new">New Build</a>
    </div>

    <!-- ── weighted-coverage score (same treatment as the build overview) ── -->
    @if (coverage(); as cov) {
      <app-coverage-score
        class="ctx-score"
        [score]="cov.pct / 100"
        [matched]="cov.matched"
        [total]="cov.total"
        [compact]="true"
      />
    }

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
            <app-view-toggle [selected]="view()" (change)="view.set($event)" />

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
                @if (editCount() > 0) {
                  <button
                    class="btn btn-sm btn-record"
                    type="button"
                    (click)="recordEdits()"
                    [disabled]="recording()"
                  >
                    {{ recording() ? 'Recording…' : 'Record ' + editCount() + ' edit' + (editCount() === 1 ? '' : 's') + ' in your dataset' }}
                  </button>
                }
              </div>
            } @else {
              <div class="panel muted">This build has no résumé payload to preview.</div>
            }
          } @else {
            <app-coverage-map
              [jd]="jd()"
              [gap]="bundle()?.gap_report ?? null"
              (act)="onCovAct($event)"
            />
          }
        </section>

        <!-- ── right: reviewer verdict + objections ── -->
        <section class="col-work">
          @if (report(); as rep) {
            <app-reviewer-rail
              [report]="rep"
              [objections]="objectionVMs()"
              [accepted]="accepted()"
              [refined]="refinedIds()"
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

    <app-refine-drawer [objection]="drawer()" (close)="drawer.set(null)" (run)="runCopilot($event)" />

    @if (toast(); as t) {
      <div class="toast on" role="status">{{ t }}</div>
    }
  `,
  styles: `
    :host { display: block; }
    .ctxbar { display: flex; align-items: flex-start; gap: 14px; margin-bottom: 20px; flex-wrap: wrap; }
    .ctx { display: flex; flex-direction: column; min-width: 0; }
    .ctx-job { font-family: var(--font-display); font-size: 22px; line-height: 1.15; margin-top: 4px; }
    .bh-kicker { font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--accent); display: flex; align-items: center; gap: 10px; }
    .bh-kicker .dot { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); }
    .bh-meta { display: flex; align-items: center; flex-wrap: wrap; gap: 8px 16px; margin-top: 10px; color: var(--muted); font-size: 14.5px; }
    .bh-meta .co { font-weight: 500; color: var(--fg); }
    .bh-meta .sep { color: var(--border-ink); opacity: 0.4; }
    .bh-src { display: inline-flex; align-items: center; gap: 6px; font-family: var(--font-mono); font-size: 12.5px; color: var(--accent); text-decoration: none; }
    .bh-src.plain { color: var(--muted); }
    .bh-src svg { width: 12px; height: 12px; }
    .ctx-status { display: flex; align-items: center; flex-wrap: wrap; gap: 8px 14px; margin-top: 12px; }
    .status-pill { display: inline-flex; align-items: center; gap: 7px; font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; padding: 5px 11px; border-radius: 999px; border: 1px solid var(--border); }
    .status-pill .sp-dot { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }
    .status-pill[data-status='Tailored'] { color: var(--accent); border-color: color-mix(in oklch, var(--accent) 40%, transparent); background: var(--accent-soft); }
    .status-pill[data-status='Exported'] { color: var(--success); border-color: color-mix(in oklch, var(--success) 35%, var(--border)); background: color-mix(in oklch, var(--success) 8%, transparent); }
    .bh-prov { font-family: var(--font-mono); font-size: 11px; color: var(--faint); letter-spacing: 0.02em; }
    .spacer { flex: 1; }
    .ctx-score { display: block; margin: 12px 0 26px; }

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
    .btn-record { border-color: color-mix(in oklch, var(--accent) 45%, var(--border)); color: var(--accent); background: var(--accent-soft); }
    .btn-record:hover:not(:disabled) { border-color: var(--accent); }

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
  protected readonly copilot = inject(CopilotHost);
  private readonly buildRunner = inject(BuildRunner);

  readonly id = input.required<string>();

  // ── raw loaded data ──────────────────────────────────────────────────
  protected readonly bundle = signal<BuildBundle | null>(null);
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
  /** Objections whose refine copilot recorded evidence this session. */
  protected readonly refinedIds = signal<ReadonlySet<string>>(new Set());
  protected readonly leftIds = signal<ReadonlySet<string>>(new Set());
  /** A layout copilot's refined human payload, previewed but not saved. */
  private readonly refinedHuman = signal<VariantPayload | null>(null);
  protected readonly busy = signal<string | null>(null);
  protected readonly drawer = signal<ObjectionVM | null>(null);
  protected readonly downloading = signal(false);
  protected readonly recording = signal(false);
  private readonly claimChecking = signal(false);
  private readonly _toast = signal<string | null>(null);
  protected readonly toast = this._toast.asReadonly();

  // ── derived view state ───────────────────────────────────────────────
  protected readonly editCount = computed(() => Object.keys(this.edits()).length);
  protected readonly jd = computed<JobRequirements | null>(() => this.bundle()?.jd ?? null);
  protected readonly report = computed<AdversarialReport | null>(() => this.bundle()?.adversarial_report ?? null);
  protected readonly coverage = computed(() => this.coverageReport());

  protected readonly jobTitle = computed(
    () => this.jd()?.title ?? this.bundle()?.canonical?.target_title ?? 'Untitled build',
  );
  protected readonly jobCompany = computed(() => this.jd()?.company ?? '');

  // Build provenance for the header — location, JD source, export status, and
  // the generation stamp (moved here when the separate overview screen was
  // collapsed into this one).
  protected readonly locationLine = computed(() => {
    const jd = this.jd();
    if (!jd) return '';
    if (jd.location) return jd.location;
    if (jd.remote && jd.remote !== 'unspecified' && jd.remote !== 'onsite') {
      return jd.remote === 'remote' ? 'Remote' : jd.remote;
    }
    return '';
  });
  protected readonly sourceUrl = computed(() => this.jd()?.source_url ?? null);
  protected readonly status = computed<'Tailored' | 'Exported'>(() =>
    (this.bundle()?.pdfs?.length ?? 0) > 0 ? 'Exported' : 'Tailored',
  );
  protected readonly provenance = computed(() => {
    const meta = this.bundle()?.meta;
    if (!meta) return null;
    const tokens =
      (meta.tailor_usage?.input_tokens ?? 0) + (meta.tailor_usage?.output_tokens ?? 0);
    return {
      created: formatStamp(meta.created_at),
      model: meta.model,
      tokens: tokens.toLocaleString(),
      template: meta.template,
    };
  });

  // The preview renders a VariantPayload. Prefer the human variant, but most
  // builds are rendered ATS-only (no human_payload.json), so fall back to the
  // ATS payload — it's the same shape, so the preview builder works on either.
  // Only when neither exists is there nothing to show.
  private readonly previewPayload = computed<VariantPayload | null>(
    () => this.refinedHuman() ?? this.bundle()?.human_payload ?? this.bundle()?.ats_payload ?? null,
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
    this.refinedIds.set(new Set());
    this.leftIds.set(new Set());
    this.refinedHuman.set(null);
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
    // `d.target` is already the domain string (`bullet:<id>`, `skills`, …), the
    // same value `objectionId` derives via `targetKey` — compare directly.
    const keys = new Set(dismissed.map((d) => `${d.target}::${d.kind}`));
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

  /** Record the current free edits back into the dataset as evidence. Only the
   *  summary and role bullets are free prose we can trace to a dataset item;
   *  skill chips are skipped (they aren't free prose). Deterministic — no LLM.
   *  The PUT is validate-gated exactly like {@link onAccept}: a 422 surfaces the
   *  findings and leaves the edits intact so nothing is silently lost. */
  protected async recordEdits(): Promise<void> {
    const ds = this.dataset();
    const editMap = this.edits();
    const keys = Object.keys(editMap);
    if (!ds || keys.length === 0) return;

    const payload = this.previewPayload();
    const next = structuredClone(ds) as ResumeDataset;

    const recorded: string[] = []; // keys we actually wrote into `next`
    const skippedSkills: string[] = [];
    const unresolved: string[] = [];

    for (const key of keys) {
      const text = editMap[key];
      if (key === 'summary') {
        next.summary = text;
        recorded.push(key);
      } else if (key.startsWith('bullet:')) {
        // bullet:${payloadRoleId}:${bulletIndex} → payload role.bullets[i].source_id
        // → the dataset bullet with that id. Any broken link is skipped, not fatal.
        const sourceId = resolveBulletSourceId(key, payload);
        const target = sourceId ? findDatasetBullet(next, sourceId) : null;
        if (!target) {
          unresolved.push(key);
          continue;
        }
        target.text = text;
        recorded.push(key);
      } else if (key.startsWith('skill:')) {
        skippedSkills.push(key);
      } else {
        unresolved.push(key);
      }
    }

    if (recorded.length === 0) {
      this.showToast(
        skippedSkills.length > 0
          ? 'Only summary and bullet edits can be recorded yet — skill edits stay local.'
          : 'Nothing to record — these edits don’t map to a dataset item.',
      );
      return;
    }

    this.recording.set(true);
    try {
      const saved = await firstValueFrom(this.api.putDataset(next));
      this.dataset.set(saved);
      // Drop the now-saved keys; anything skipped/unresolved stays a local edit.
      this.edits.update((m) => {
        const copy = { ...m };
        for (const k of recorded) delete copy[k];
        return copy;
      });
      // Re-run the deterministic checks against the enriched dataset.
      await this.recompute();

      // Never-fabricate honesty: a recorded line may still not trace (e.g. the
      // edit diverged from every evidence phrase). Warn, but never block the save.
      const provMap = provenanceIndex(this.provReport());
      const stillUnrecorded = recorded.filter((k) => provMap.get(k)?.status === 'unrecorded');

      let msg =
        stillUnrecorded.length > 0
          ? `Recorded, but ${plural(stillUnrecorded.length, 'line')} no longer trace to your evidence — review them.`
          : `Recorded ${plural(recorded.length, 'edit')} to your dataset.`;
      if (skippedSkills.length > 0) {
        msg += ` ${plural(skippedSkills.length, 'skill edit')} skipped — skills aren’t recordable yet.`;
      }
      this.showToast(msg);
    } catch (err) {
      // Same 422 path as onAccept: surface validation findings, keep the edits.
      const msg =
        err instanceof HttpErrorResponse && err.status === 422
          ? `Validation blocked the save: ${findings(err)}`
          : errMessage(err);
      this.showToast(msg);
    } finally {
      this.recording.set(false);
    }
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

  /** Run the copilot behind an objection through {@link CopilotHost} (which shows
   *  the busy/progress overlay and drives the Q&A modal). Dataset-enriching
   *  copilots record evidence and PUT the dataset; the layout copilot returns a
   *  projected human payload we preview in-session only. Everything is guarded so
   *  a thrown export or a cancelled interview surfaces a toast, never wedges. */
  protected async runCopilot(o: ObjectionVM): Promise<void> {
    // Only one copilot at a time — a second would race the first for the shared
    // Q&A modal and hang it (see CopilotHost.ask). Refuse before we start.
    if (this.copilot.running()) {
      this.showToast('A copilot is already running — finish or dismiss it first.');
      return;
    }
    const ds = this.dataset();
    if (!ds) {
      this.showToast('Dataset unavailable — cannot run a copilot.');
      return;
    }
    // A single-objection report keeps the copilot's work surgical.
    const rep = this.report();
    const single: AdversarialReport = {
      objections: [o.objection],
      overall_score: rep?.overall_score ?? 0,
      persona_notes: rep?.persona_notes ?? '',
    };
    this.drawer.set(null);

    try {
      const result = await this.copilot.runWithUi(`${o.copilot} copilot`, (): Promise<unknown> => {
        switch (o.copilot) {
          case 'strengthen':
            return this.wasm.strengthen(ds, single);
          case 'metric':
            return this.wasm.captureMetrics(ds, single);
          case 'summary':
            return this.wasm.refineSummary(ds, o.objection.message);
          case 'skills': {
            const jd = this.jd();
            const gap = this.bundle()?.gap_report;
            if (!jd || !gap) {
              throw new Error('This build has no JD or gap report to run the skill check against.');
            }
            return this.wasm.verifySkills(ds, jd, gap);
          }
          case 'layout': {
            const jd = this.jd();
            const canonical = this.bundle()?.canonical;
            if (!jd || !canonical) {
              throw new Error('This build has no canonical draft to refine the layout from.');
            }
            return this.wasm.refineLayout(canonical, ds, jd, o.objection);
          }
        }
      });

      if (o.copilot === 'layout') {
        // Layout is presentation-only: preview the refined variant, never touch
        // the canonical claims or the saved build.
        this.refinedHuman.set(result as VariantPayload);
        // `edits` is keyed by the OLD payload's positional `bullet:role:index`;
        // the new payload re-projects those positions, so a stale key could write
        // an edit onto the wrong dataset bullet. Drop pending edits on the swap.
        this.edits.set({});
        this.showToast('Layout refined — preview updated (not saved to this build). Unsaved edits were cleared by the layout change.');
        return;
      }

      await this.applyDatasetRefine(o, result);
    } catch (err) {
      this.showToast(errMessage(err));
    }
  }

  /** Apply a dataset-enriching copilot's result: PUT the enriched dataset
   *  (validate-gated, 422 surfaces findings), re-run the deterministic checks,
   *  mark the objection refined, and toast a plain summary. A copilot the user
   *  skipped through (no counts) records nothing and stays open. */
  private async applyDatasetRefine(o: ObjectionVM, result: unknown): Promise<void> {
    const r = (result ?? {}) as RefineResult;
    const summary = refineSummaryText(o.copilot, r);
    if (!summary || !r.dataset) {
      this.showToast('No changes recorded.');
      return;
    }
    await this.persistEnrichedDataset(r.dataset, summary, [o.id]);
  }

  /** PUT an enriched dataset (validate-gated, 422 surfaces findings), re-run the
   *  deterministic checks, and mark the given objections refined. Shared by the
   *  objection copilots and the coverage-map skills session. */
  private async persistEnrichedDataset(
    dataset: ResumeDataset,
    summary: string,
    markIds: string[],
  ): Promise<void> {
    try {
      const saved = await firstValueFrom(this.api.putDataset(dataset));
      this.dataset.set(saved);
      // Provenance may now trace a previously-unrecorded line.
      void this.recompute();
      this.refinedIds.update((s) => markIds.reduce((acc, id) => withAdded(acc, id), s));
      this.leftIds.update((s) => markIds.reduce((acc, id) => withRemoved(acc, id), s));
      this.showToast(summary);
    } catch (err) {
      const msg =
        err instanceof HttpErrorResponse && err.status === 422
          ? `Validation blocked the save: ${findings(err)}`
          : errMessage(err);
      this.showToast(msg);
    }
  }

  /** The skill-verification copilot behind a coverage-map row: it interviews
   *  for the ONE requirement the user clicked (not the whole gap) — "do you have
   *  this? then talk me through it" — and records the evidence you confirm. */
  private async runSkillGap(name: string): Promise<void> {
    if (this.copilot.running()) {
      this.showToast('A copilot is already running — finish or dismiss it first.');
      return;
    }
    const ds = this.dataset();
    const jd = this.jd();
    if (!ds || !jd) {
      this.showToast('This build has no JD to verify against.');
      return;
    }
    try {
      const result = await this.copilot.runWithUi('skills copilot', (): Promise<unknown> =>
        this.wasm.verifySkill(ds, jd, name),
      );
      const r = (result ?? {}) as RefineResult;
      const summary = refineSummaryText('skills', r);
      if (!summary || !r.dataset) {
        this.showToast(`Nothing recorded for “${name}”.`);
        return;
      }
      // Mark any open skills objection that names this requirement as refined.
      const needle = name.toLowerCase();
      const ids = this.objectionVMs()
        .filter((o) => o.copilot === 'skills' && o.targetLabel.toLowerCase().includes(needle))
        .map((o) => o.id);
      await this.persistEnrichedDataset(r.dataset, summary, ids);
    } catch (err) {
      this.showToast(errMessage(err));
    }
  }

  /** A coverage-map row's action (Refine / Strengthen / Fill the gap). If an
   *  open objection already targets this requirement, run its copilot directly;
   *  otherwise a gap/semantic row opens the skill-verification session to record
   *  the evidence for it, and an already-matched row just says so. */
  protected onCovAct(e: { name: string; intent: 'matched' | 'semantic' | 'gap' }): void {
    const needle = e.name.toLowerCase();
    const match = this.objectionVMs().find(
      (o) =>
        !this.refinedIds().has(o.id) &&
        !this.accepted().has(o.id) &&
        (o.targetLabel.toLowerCase().includes(needle) ||
          (o.flaggedText?.toLowerCase().includes(needle) ?? false)),
    );
    if (match) {
      void this.runCopilot(match);
      return;
    }
    if (e.intent === 'matched') {
      this.showToast(`“${e.name}” is already covered by your evidence.`);
      return;
    }
    // A bare gap/semantic requirement → verify exactly this one, scoped to what
    // the user clicked (not the whole-gap checklist).
    void this.runSkillGap(e.name);
  }

  /** Accept as intentional → append a `DismissedObjection` to the dataset and
   *  PUT it (validate-gated). A 422 surfaces the validation findings. */
  protected onAccept(o: ObjectionVM): void {
    const ds = this.dataset();
    if (!ds) {
      this.showToast('Dataset unavailable — cannot persist this dismissal.');
      return;
    }
    // Persist the DOMAIN target string (`bullet:<id>`, `skills`, `summary`, …),
    // not the raw wire `ObjectionTarget` — that's what Rust's `DismissedObjection`
    // deserializes, and it keeps the dismissal id-consistent with `objectionId`.
    const next = withDismissal(ds, { target: targetKey(o.objection.target), kind: o.objection.kind });
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

  // ── retailor ─────────────────────────────────────────────────────────
  /** Regenerate this build's résumé: re-run the adversarial loop for the same
   *  JD against the user's *current* (possibly copilot-enriched) dataset via the
   *  shared {@link BuildRunner}, saving a fresh build and navigating to it. The
   *  overlay refuses a concurrent run; every failure surfaces a toast, never a
   *  dead spinner. */
  protected async retailor(): Promise<void> {
    if (this.copilot.running()) {
      this.showToast('A copilot is already running — finish or dismiss it first.');
      return;
    }
    const jd = this.bundle()?.jd;
    if (!jd) {
      this.showToast('This build has no parsed job description to retailor.');
      return;
    }
    try {
      let dataset: ResumeDataset;
      try {
        dataset = await firstValueFrom(this.api.getDataset());
      } catch (err) {
        if (err instanceof HttpErrorResponse && err.status === 404) {
          this.showToast('No dataset yet.');
          return;
        }
        throw err;
      }
      // A cancelled run is confirmed by the app-global notice BuildRunner fires
      // after navigation (survives this component unmounting) — no local toast.
      await this.buildRunner.runAndSave(jd, dataset, 'Retailor');
    } catch (err) {
      this.showToast(errMessage(err));
    }
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
  /** The domain target string (`bullet:<id>`, `skills`, `summary`, `layout`,
   *  `overall`) — matches Rust's `DismissedObjection.target: String`. */
  target: string;
  kind: ObjectionVM['objection']['kind'];
}

/** The union of shapes the dataset-enriching copilots return (all fields
 *  optional; each copilot populates the counts relevant to it). */
interface RefineResult {
  dataset?: ResumeDataset;
  changed?: boolean;
  added?: number;
  verified?: number;
  removed?: number;
  skipped?: number;
  bullets_added?: number;
  declined?: number;
}

const plural = (n: number, word: string): string => `${n} ${word}${n === 1 ? '' : 's'}`;

/** A plain human summary from a copilot's counts, or null if it recorded nothing
 *  (the user skipped every prompt). Layout never reaches here. */
function refineSummaryText(kind: CopilotKind, r: RefineResult): string | null {
  switch (kind) {
    case 'strengthen':
      return r.changed ? 'Recorded a stronger line as evidence — it lands on your next build.' : null;
    case 'summary':
      return r.changed ? 'Updated summary saved to your dataset.' : null;
    case 'metric':
      return (r.added ?? 0) > 0 ? `Recorded ${plural(r.added ?? 0, 'metric')} as evidence.` : null;
    case 'skills': {
      const parts: string[] = [];
      if (r.verified) parts.push(`verified ${plural(r.verified, 'skill')}`);
      if (r.bullets_added) parts.push(`added ${plural(r.bullets_added, 'bullet')}`);
      if (r.removed) parts.push(`removed ${plural(r.removed, 'skill')}`);
      if (parts.length === 0) return null;
      const s = parts.join(', ');
      return `${s.charAt(0).toUpperCase()}${s.slice(1)}.`;
    }
    case 'layout':
      return null;
  }
}

/** Resolve a `bullet:${payloadRoleId}:${index}` preview key to the dataset
 *  bullet id it projects: find the payload role by `id`, take that bullet's
 *  `source_id` (which is the dataset bullet id). Role ids may themselves contain
 *  colons, so the index is split off the end. Returns null if unresolvable. */
function resolveBulletSourceId(key: string, payload: VariantPayload | null): string | null {
  const rest = key.slice('bullet:'.length);
  const lastColon = rest.lastIndexOf(':');
  if (lastColon < 0) return null;
  const roleId = rest.slice(0, lastColon);
  const index = Number(rest.slice(lastColon + 1));
  if (!Number.isInteger(index) || index < 0) return null;
  const role = payload?.roles?.find((r) => r.id === roleId);
  return role?.bullets?.[index]?.source_id ?? null;
}

/** Find the dataset bullet with the given id across all roles. */
function findDatasetBullet(ds: ResumeDataset, sourceId: string): Bullet | null {
  for (const role of ds.roles ?? []) {
    for (const b of role.bullets ?? []) {
      if (b.id === sourceId) return b;
    }
  }
  return null;
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
  // Both targets are already normalized domain strings — compare directly.
  const exists = list.some((x) => x.target === d.target && x.kind === d.kind);
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
  if (err instanceof HttpErrorResponse) {
    // Server envelope is `{ error: { kind, message } }`; some paths send a bare
    // string or `{ message }`. Read the real message, not the generic HTTP one.
    const b = err.error as { error?: { message?: string }; message?: string } | string | null;
    if (typeof b === 'string') return b || err.message;
    return b?.error?.message ?? b?.message ?? err.message;
  }
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err; // wasm rejects with plain strings
  return 'request failed';
}

const MONTHS = [
  'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
  'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec',
];

/** A build's `created_at` stamp as "Jun 25, 2026 · 09:43" (falls back to the
 *  raw ISO string if it doesn't parse). */
function formatStamp(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  return `${MONTHS[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()} · ${hh}:${mm}`;
}

function findings(err: HttpErrorResponse): string {
  // The 422 validation body carries the failures under `problems` (an array of
  // strings), alongside the `{ error }` envelope. Join them; otherwise fall back
  // to the envelope's message via errMessage.
  const problems = (err.error as { problems?: unknown } | null)?.problems;
  if (Array.isArray(problems) && problems.length > 0) {
    return problems.map(String).join('; ');
  }
  return errMessage(err);
}
