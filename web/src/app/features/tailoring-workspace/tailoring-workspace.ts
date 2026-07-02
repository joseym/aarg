import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
  untracked,
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
import { PdfPreview } from './pdf-preview';
import { ReviewerRail } from './reviewer-rail';
import { RefineDrawer } from './refine-drawer';
import { EditBar, type EditLogRow } from './edit-bar';
import { CoverageMap } from '../../shared/coverage-map';
import { ViewToggle } from '../../shared/view-toggle';
import { ScorePanel } from '../../shared/score-panel';
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

/** One entry in a build's on-disk `edit_log.json`: a workspace edit saved into
 *  the build. `prev`/`next` are the text before/after; a Revert re-posts `prev`
 *  as an inverse edit (which appends its own entry — the log is an audit trail,
 *  never rewritten). Mirrors the server's `EditLogEntry`. */
interface EditLogEntry {
  at: string;
  target: string;
  prev: string;
  next: string;
}

/** `BuildDetail` (models/build.ts) does not yet type a human `VariantPayload`
 *  or the edit log; the serve bundle carries both on disk. Read them defensively
 *  and flag the gap rather than editing the shared model. */
interface BuildBundle extends BuildDetail {
  human_payload?: VariantPayload;
  edit_log?: EditLogEntry[];
}

type ClaimState = 'ok' | 'checking' | 'flag';

/** The tailoring workspace (`/build/:id/tailor`): a two-pane screen — the
 *  editable, provenance-aware résumé preview on the left; the reviewer's verdict
 *  and objection triage on the right. This wave wires read + deterministic
 *  (wasm) paths; the LLM copilots are stubbed behind {@link RefineDrawer}. */
@Component({
  selector: 'app-tailoring-workspace',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '(document:keydown)': 'onKeydown($event)' },
  imports: [RouterLink, ResumePreview, PdfPreview, CoverageMap, ViewToggle, ReviewerRail, RefineDrawer, ScorePanel, EditBar],
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

    <!-- ── one score language: every build metric in a single explained panel ── -->
    @if (coverage(); as cov) {
      <app-score-panel
        class="ctx-score"
        [weighted]="cov.pct / 100"
        [verdict]="report()?.overall_score ?? null"
        [ats]="bundle()?.ats_report?.coverage ?? null"
        [matched]="cov.matched"
        [total]="cov.total"
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
        <section class="col-preview" [class.bar-open]="showEditBar()">
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
                  Click the badge to jump to a flagged line — confirm it as your
                  own evidence, or edit it.
                </div>
              }
            </div>
          </div>

          @if (view() === 'preview') {
            <!-- ── fidelity sub-toggle: editable HTML vs the real Typst PDF ── -->
            <div class="pv-sub">
              <div class="segmented" role="tablist" aria-label="Preview fidelity">
                <button
                  type="button" role="tab"
                  [class.on]="previewMode() === 'editing'"
                  [attr.aria-selected]="previewMode() === 'editing'"
                  (click)="previewMode.set('editing')"
                >Editing</button>
                <button
                  type="button" role="tab"
                  [class.on]="previewMode() === 'pixel'"
                  [attr.aria-selected]="previewMode() === 'pixel'"
                  (click)="previewMode.set('pixel')"
                >Pixel-perfect</button>
              </div>
              @if (templateOptions().length > 0) {
                <label class="tpl">
                  <span class="tpl-label">Template:</span>
                  <select [value]="chosenTemplate()" (change)="onTemplateChange($event)">
                    @for (t of templateOptions(); track t) {
                      <!-- [selected] as well as the select's [value]: when the async
                           template list replaces the options after first render, a
                           bare [value] binding isn't re-applied and the select can
                           DISPLAY the wrong option; [selected] tracks the signal. -->
                      <option [value]="t" [selected]="t === chosenTemplate()">{{ t }}</option>
                    }
                  </select>
                </label>
              }
            </div>

            <!-- The PDF preview mounts on the FIRST Pixel-perfect entry, then
                 stays mounted across Editing↔Pixel flips (hidden, never
                 destroyed) so its per-template render cache survives a mode
                 toggle instead of re-hitting Typst for an unchanged
                 payload/template. -->
            @if (effectivePayload(); as pp) {
              @if (pixelSeen()) {
                <app-pdf-preview
                  [class.hide]="previewMode() !== 'pixel'"
                  [variant]="pp.variant"
                  [payload]="pp"
                  [template]="chosenTemplate() ?? ''"
                  (error)="onPreviewError($event)"
                />
              }
            } @else if (previewMode() === 'pixel') {
              <div class="panel muted">This build has no résumé payload to preview.</div>
            }

            @if (previewMode() === 'editing') {
              @if (previewModel(); as m) {
                <app-resume-preview
                  [model]="m"
                  [highlightKey]="highlightKey()"
                  (edit)="onEdit($event)"
                  (confirm)="onConfirmLine($event)"
                />
                <!-- The pending-edit actions (Undo / Record / Save) and the edit
                     history now live in the slide-up sticky app-edit-bar below —
                     this bar keeps only the mode concerns (fidelity + pixel link). -->
                <div class="fidelity">
                  <span class="tag">HTML · editing</span>
                  <span>Live in-browser preview — edit any line. Facts stay identical to the</span>
                  <button class="btn btn-sm" type="button" (click)="previewMode.set('pixel')">pixel-perfect PDF</button>
                </div>
              } @else {
                <div class="panel muted">This build has no résumé payload to preview.</div>
              }
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

    <!-- ── slide-up sticky pending-edits bar. Its single visibility rule is
         showEditBar() (pending edits / undoable history, editing preview, no
         modal up); hidden it is opacity-0 and pointer-events:none, so it can
         stay mounted for the slide-out transition. ── -->
    <app-edit-bar
      [visible]="showEditBar()"
      [editCount]="editCount()"
      [canUndo]="editHistory().length > 0"
      [recording]="recording()"
      [saving]="saving()"
      [logRows]="editLogRows()"
      (undo)="undoEdit()"
      (record)="recordEdits()"
      (save)="saveEdits()"
      (revert)="revertEdit($event)"
    />

    @if (toast(); as t) {
      <!-- "raised" lifts the toast clear of the pending-edits bar so it never
           covers the bar's controls while the bar is slid in. -->
      <div class="toast on" [class.raised]="showEditBar()" role="status">{{ t }}</div>
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
    .ctx-score { display: block; width: 100%; margin: 12px 0 26px; }

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

    .pv-sub { display: flex; align-items: center; gap: 12px; margin: 14px 0 16px; flex-wrap: wrap; }
    .pv-sub .segmented { display: inline-flex; padding: 3px; gap: 3px; background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px; }
    .pv-sub .segmented button { display: inline-flex; align-items: center; padding: 6px 14px; border: 0; border-radius: 999px; background: transparent; font: inherit; font-size: 13px; font-weight: 500; color: var(--muted); cursor: pointer; transition: background 0.15s, color 0.15s, box-shadow 0.15s; }
    .pv-sub .segmented button.on { background: var(--surface); color: var(--fg); box-shadow: 0 1px 2px color-mix(in oklch, var(--fg) 12%, transparent); }
    .pv-sub .segmented button:not(.on):hover { color: var(--fg); }
    .pv-sub .segmented button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .tpl { display: inline-flex; align-items: center; gap: 8px; font-size: 12.5px; color: var(--muted); }
    .tpl-label { font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.04em; text-transform: uppercase; color: var(--faint); }
    .tpl select { height: 30px; padding: 0 8px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 13px; color: inherit; cursor: pointer; }
    .tpl select:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    /* Hides the still-mounted PDF preview in Editing mode. Must out-specify the
       child's own \`:host { display: block }\` (0,1,0) — element+class is (0,1,1). */
    app-pdf-preview.hide { display: none; }

    .fidelity { display: flex; align-items: center; gap: 10px; margin-top: 14px; font-size: 12.5px; color: var(--muted); flex-wrap: wrap; }
    .fidelity .tag { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.1em; text-transform: uppercase; color: var(--faint); border: 1px solid var(--border); border-radius: 999px; padding: 3px 8px; }

    /* Reserve room for the fixed pending-edits bar so it never permanently
       covers the last preview line while it's slid in. */
    .col-preview.bar-open { padding-bottom: 96px; }

    .panel { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 22px; max-width: 62ch; display: flex; flex-direction: column; gap: 10px; align-items: flex-start; }
    .muted { color: var(--muted); }

    /* pointer-events: none — a toast is a passive status flash; it must never
       intercept clicks meant for what's beneath it (e.g. history-popover rows). */
    .toast { position: fixed; bottom: 24px; left: 50%; transform: translateX(-50%); z-index: 80; pointer-events: none; background: var(--fg); color: oklch(97% 0.01 80); padding: 12px 18px; border-radius: 11px; font-size: 13.5px; box-shadow: 0 16px 40px -16px color-mix(in oklch, var(--fg) 60%, transparent); }
    /* While the pending-edits bar is slid in (bottom: 18px + ~72px tall), lift
       the toast above it — a toast must never cover the bar's controls. */
    .toast.raised { bottom: 118px; }

    @media (max-width: 1080px) {
      .work { grid-template-columns: 1fr; }
      .col-preview { border-right: 0; border-bottom: 1px solid var(--border); padding: 4px 0 32px; }
      .col-work { position: static; padding: 26px 0 40px; }
      /* Narrow widths let the bar's actions wrap to a second row (~120px tall):
         reserve more room under the preview and lift the toast further. */
      .col-preview.bar-open { padding-bottom: 156px; }
      .toast.raised { bottom: 172px; }
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
  /** The Final-preview fidelity sub-toggle: the editable HTML preview
   *  (`editing`) or the real Typst-rendered PDF in an iframe (`pixel`). */
  protected readonly previewMode = signal<'editing' | 'pixel'>('editing');
  /** Latches true the first time Pixel-perfect is entered (per build). The PDF
   *  preview mounts lazily on that first entry — no Typst render for a build
   *  the user never inspects — but is then kept mounted and merely hidden on a
   *  flip back to Editing, so its render cache survives mode toggles. */
  protected readonly pixelSeen = signal(false);
  /** The templates resolvable per variant (`GET /api/templates`) — feeds the
   *  picker. `null` until loaded (or if the fetch fails, in which case the
   *  picker stays hidden and rendering falls back to the payload's own stamp). */
  private readonly templates = signal<{ ats: string[]; human: string[] } | null>(null);
  /** The template the picker has selected (a bare name like `modern`), per
   *  session — not persisted. Defaults to the preview payload's own stamp; see
   *  the reset effect in the constructor. Drives BOTH the pixel-perfect iframe
   *  and Download PDF. */
  protected readonly chosenTemplate = signal<string | null>(null);
  protected readonly infoOpen = signal(false);
  /** The line key the preview should spotlight (badge → jump). Cycles through
   *  {@link unrecordedKeys} on repeated badge clicks. */
  protected readonly highlightKey = signal<string | null>(null);
  /** Which unrecorded line the next badge click jumps to. Reset to 0 whenever the
   *  set of flagged lines changes (see the constructor effect). */
  private cycleIndex = 0;
  protected readonly edits = signal<Record<string, string>>({});
  /** Session undo stack for local edits. Each entry captures the overlay's value
   *  BEFORE the edit (`prev`): `null` means the key wasn't overlaid (the payload's
   *  original text was showing), so undo removes the key; otherwise undo restores
   *  `prev`. Capped at {@link EDIT_HISTORY_CAP}; cleared by {@link reset} and by the
   *  layout swap that drops pending edits. */
  protected readonly editHistory = signal<Array<{ key: string; prev: string | null; next: string }>>([]);
  /** Edit keys already recorded to the dataset this session. Kept IN `edits` (so
   *  the preview text persists rather than snapping back), but excluded from the
   *  pending `editCount` — re-editing a key re-pends it (see {@link onEdit}). */
  protected readonly recordedKeys = signal<ReadonlySet<string>>(new Set());
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
  /** A save-into-build or revert is in flight (both POST `/builds/:id/edits`). */
  protected readonly saving = signal(false);
  private readonly claimChecking = signal(false);
  private readonly _toast = signal<string | null>(null);
  protected readonly toast = this._toast.asReadonly();

  // ── derived view state ───────────────────────────────────────────────
  // Pending edits drive the Record button: an edit already recorded stays in the
  // preview (in `edits`) but no longer counts as pending.
  protected readonly editCount = computed(() => {
    const recorded = this.recordedKeys();
    return Object.keys(this.edits()).filter((k) => !recorded.has(k)).length;
  });
  protected readonly jd = computed<JobRequirements | null>(() => this.bundle()?.jd ?? null);
  protected readonly report = computed<AdversarialReport | null>(() => this.bundle()?.adversarial_report ?? null);
  protected readonly coverage = computed(() => this.coverageReport());
  /** The build's on-disk edit history (empty until an edit is saved into it). */
  protected readonly editLog = computed<EditLogEntry[]>(() => this.bundle()?.edit_log ?? []);

  /** The ONE visibility rule for the slide-up pending-edits bar: there is
   *  something to act on (pending edits OR undoable session history), we're in
   *  the editable HTML preview, AND no modal surface is up — a fixed toolbar
   *  must not float above (or stay interactive under) modal surfaces, so the
   *  refine drawer and the copilot overlay / Q&A modal each hide it. Gates the
   *  bar's slide-in, the preview's bottom padding, and the raised toast. */
  protected readonly showEditBar = computed(
    () =>
      (this.editCount() > 0 || this.editHistory().length > 0) &&
      this.view() === 'preview' &&
      this.previewMode() === 'editing' &&
      this.drawer() === null &&
      !this.copilot.running() &&
      !this.copilot.question(),
  );

  /** The build's on-disk edit log, pre-formatted for the bar's history popover.
   *  The bar stays presentational — it renders these strings and emits the raw
   *  `entry` back on Revert. */
  protected readonly editLogRows = computed<EditLogRow[]>(() =>
    this.editLog().map((e) => ({
      label: this.editTargetLabel(e.target),
      time: this.editEntryTime(e.at),
      prevText: this.truncate(e.prev),
      nextText: this.truncate(e.next),
      entry: e,
    })),
  );

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
  protected readonly previewPayload = computed<VariantPayload | null>(
    () => this.refinedHuman() ?? this.bundle()?.human_payload ?? this.bundle()?.ats_payload ?? null,
  );

  /** The preview payload with the user's local edits applied — the single object
   *  fed to BOTH the pixel-perfect renderer and Download PDF, so what the preview
   *  shows and what downloads are byte-identical.
   *
   *  When there are no edits it returns {@link previewPayload} UNCHANGED (same
   *  reference), so the PDF preview's per-payload render cache stays warm. An edit
   *  produces a fresh `structuredClone`; pdf-preview clears its cache on any
   *  payload-reference change, so the edit re-renders through Typst for free.
   *  Unresolvable keys are skipped silently (same tolerance as {@link recordLines}).
   *
   *  HONESTY: this applies the edit to the rendered projection ONLY — it does NOT
   *  persist to the stored build (a Phase B adds that). The claim-check overlay
   *  (provenance / "your own edit" pills) carries the never-fabricate story for
   *  these local edits. */
  protected readonly effectivePayload = computed<VariantPayload | null>(() => {
    const base = this.previewPayload();
    const edits = this.edits();
    // No edits → SAME reference, so pdf-preview's render cache is untouched.
    if (!base || Object.keys(edits).length === 0) return base;
    const next = structuredClone(base) as VariantPayload;
    for (const [key, text] of Object.entries(edits)) {
      if (key === 'summary') {
        next.summary = text;
      } else if (key.startsWith('bullet:')) {
        // bullet:<roleId>:<i> — role ids may contain colons, so split the index
        // off the end (mirrors resolveBulletSourceId).
        const rest = key.slice('bullet:'.length);
        const lastColon = rest.lastIndexOf(':');
        if (lastColon < 0) continue;
        const roleId = rest.slice(0, lastColon);
        const index = Number(rest.slice(lastColon + 1));
        if (!Number.isInteger(index) || index < 0) continue;
        const bullet = next.roles?.find((r) => r.id === roleId)?.bullets?.[index];
        if (bullet) bullet.text = text; // unresolvable → skip silently
      } else if (key.startsWith('skill:')) {
        const index = Number(key.slice('skill:'.length));
        if (!Number.isInteger(index) || index < 0) continue;
        const skills = next.skills_section?.skills;
        if (skills && index < skills.length) skills[index] = text;
      }
      // any other key shape → skipped silently
    }
    return next;
  });

  /** The template names to offer for the CURRENT preview payload's variant —
   *  ATS names for an ATS payload, human names for a human one. Degrades
   *  gracefully against a server without `GET /api/templates` (an older binary
   *  404s it) or a response with no entry for this variant: the picker then
   *  offers just the payload's own bare template stamp, so the select is never
   *  empty and pixel-perfect / Download keep working. Silent by design — a
   *  capability degrade, not a user error, so no toast. The full server list
   *  wins whenever the endpoint answers with names for this variant. */
  protected readonly templateOptions = computed<string[]>(() => {
    const payload = this.previewPayload();
    if (!payload) return [];
    const t = this.templates();
    const list = t ? (payload.variant === 'ats' ? t.ats : t.human) : [];
    if (list.length > 0) return list;
    // Endpoint errored/absent or listed nothing for this variant — seed with
    // the build's actual template (its stamp minus the `ats/`/`human/` prefix).
    const stamp = payload.template?.replace(/^(ats|human)\//, '');
    return stamp ? [stamp] : [];
  });

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

  /** The ordered keys of lines the badge flags — the exact predicate
   *  {@link claimState} uses (unrecorded, not overridden by a pending edit). The
   *  badge jump cycles through these; the count drives {@link claimText}. */
  protected readonly unrecordedKeys = computed<string[]>(() => {
    const edits = this.edits();
    return (this.provReport()?.lines ?? [])
      .filter((l) => l.status === 'unrecorded' && !Object.prototype.hasOwnProperty.call(edits, keyOf(l)))
      .map((l) => keyOf(l));
  });

  private readonly flaggedCount = computed(() => this.unrecordedKeys().length);

  protected readonly claimText = computed(() => {
    switch (this.claimState()) {
      case 'checking':
        return 'Checking every line against your evidence…';
      case 'flag': {
        const n = this.flaggedCount();
        return `${n || 1} line${n === 1 ? '' : 's'} need${n === 1 ? 's' : ''} your confirmation before ${n === 1 ? 'it lands' : 'they land'}`;
      }
      default:
        return 'Every line traces to your evidence';
    }
  });

  constructor() {
    // The templates the picker can offer. A failure just leaves the picker
    // hidden (empty options) — the preview and download fall back to each
    // payload's own template stamp, so the screen still works keyless.
    this.api.getTemplates().subscribe({
      next: (t) => this.templates.set(t),
      error: () => {},
    });

    // Default the picker to whichever template the current payload was stamped
    // with, resetting whenever the payload itself changes (a fresh build, or a
    // layout copilot swapping in a re-projected human variant). A manual pick
    // survives unrelated recomputes because the payload reference is stable.
    effect(() => {
      const payload = this.previewPayload();
      const stamp = payload?.template ? payload.template.replace(/^(ats|human)\//, '') : null;
      untracked(() => this.chosenTemplate.set(stamp));
    });

    // Latch the lazy PDF-preview mount on the first Pixel-perfect entry.
    effect(() => {
      if (this.previewMode() === 'pixel') this.pixelSeen.set(true);
    });

    // Whenever the set of flagged lines changes (a re-check, an edit, a confirm),
    // restart the jump cycle from the first flagged line.
    effect(() => {
      this.unrecordedKeys();
      this.cycleIndex = 0;
    });

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
    this.editHistory.set([]);
    this.recordedKeys.set(new Set());
    this.accepted.set(new Set());
    this.refinedIds.set(new Set());
    this.leftIds.set(new Set());
    this.refinedHuman.set(null);
    this.drawer.set(null);
    this.view.set('preview');
    this.previewMode.set('editing');
    // Unmount the PDF preview for the incoming build (its onDestroy revokes the
    // old build's cached object URLs); it re-mounts on the next pixel entry.
    this.pixelSeen.set(false);
  }

  /** The picker changed the template: re-project the pixel-perfect iframe and
   *  the next Download PDF against the newly-chosen (bare) name. */
  protected onTemplateChange(e: Event): void {
    this.chosenTemplate.set((e.target as HTMLSelectElement).value);
  }

  /** A pixel-perfect render failed — surface it the same way every other
   *  failure on this screen does, via the shared toast + `errMessage`. */
  protected onPreviewError(err: unknown): void {
    this.showToast(`Render failed: ${errMessage(err)}`);
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
    const overlay = this.edits();
    const had = Object.prototype.hasOwnProperty.call(overlay, e.key);
    const prev = had ? overlay[e.key] : null;
    // No-op re-blur of an already-overlaid line with identical text: nothing
    // changed, so don't grow the undo stack or re-run the checks.
    if (prev === e.text) return;
    // Push the pre-edit overlay value onto the undo stack (capped).
    this.editHistory.update((h) => {
      const next = [...h, { key: e.key, prev, next: e.text }];
      return next.length > EDIT_HISTORY_CAP ? next.slice(next.length - EDIT_HISTORY_CAP) : next;
    });
    this.edits.update((m) => ({ ...m, [e.key]: e.text }));
    // A re-edit of an already-recorded line is pending again until re-recorded.
    this.recordedKeys.update((s) => withRemoved(s, e.key));
    // Per the UI decision: re-run the deterministic provenance check on edit.
    // The edited line is shown as "your own edit" locally; a later wave will
    // rebuild an edited canonical so the core can re-check the changed line too.
    void this.recompute();
    // No toast here: the pending-edits bar sliding up IS the "kept locally"
    // signal, with the save/record affordances right on it.
  }

  /** Undo the most recent local edit. If the key wasn't overlaid before the edit
   *  (`prev === null`) the overlay entry is removed (the line reverts to the
   *  payload's original text); otherwise it's restored to `prev`. A key that was
   *  recorded to the dataset this session loses its recorded mark AND gets an
   *  honest caveat — reverting the local overlay does NOT touch the dataset copy;
   *  a dataset revert is a separate, deliberate act. Re-runs the deterministic
   *  checks so provenance reflects the revert. */
  protected undoEdit(): void {
    const hist = this.editHistory();
    if (hist.length === 0) return;
    const last = hist[hist.length - 1];
    this.editHistory.set(hist.slice(0, -1));
    if (last.prev === null) {
      this.edits.update((m) => {
        const next = { ...m };
        delete next[last.key];
        return next;
      });
    } else {
      this.edits.update((m) => ({ ...m, [last.key]: last.prev as string }));
    }
    if (this.recordedKeys().has(last.key)) {
      this.recordedKeys.update((s) => withRemoved(s, last.key));
      this.showToast('Reverted locally — the version recorded in your dataset is unchanged.');
    } else {
      this.showToast('Edit undone.');
    }
    void this.recompute();
  }

  /** Workspace-level undo shortcut. The browser's native undo owns text editing
   *  while a contenteditable line is focused, so we only claim Cmd/Ctrl+Z when the
   *  event target is NOT inside an editable line. */
  protected onKeydown(event: KeyboardEvent): void {
    const mod = event.metaKey || event.ctrlKey;

    // Cmd/Ctrl+S saves the pending edits into this build. Claimed only in the
    // editable HTML preview — anywhere else the browser default stands.
    if (mod && !event.shiftKey && (event.key === 's' || event.key === 'S')) {
      if (this.view() !== 'preview' || this.previewMode() !== 'editing') return;
      // A modal surface (refine drawer, copilot Q&A) is not an editor context:
      // the bar hides there, and the shortcut it fronts must die with it — a
      // save mid-drawer would reloadBundle() under the open dialog's feet.
      // Plain return: the browser default stands, per this branch's philosophy.
      if (this.drawer() !== null || this.copilot.running() || this.copilot.question()) return;
      // Keydown auto-repeats while the chord is held: swallow repeats while a
      // save/record is already in flight so we never fire concurrent POSTs —
      // and never let the browser Save dialog pop mid-save.
      if (this.saving() || this.recording()) {
        event.preventDefault();
        return;
      }
      // A focused line may hold an uncommitted edit, so check focus BEFORE
      // counting: preventDefault first (we're in an editor context — hijacking
      // Cmd+S is correct even if the commit turns out to be a no-op), then blur
      // — the line's blur handler emits its edit synchronously — THEN evaluate
      // editCount against the committed overlay.
      const active = document.activeElement as HTMLElement | null;
      if (active?.isContentEditable) {
        event.preventDefault();
        active.blur();
        if (this.editCount() === 0) return; // no-op commit — dialog already suppressed
        void this.saveEdits();
        return;
      }
      if (this.editCount() === 0) return; // nothing to save — default stands
      event.preventDefault();
      void this.saveEdits();
      return;
    }

    const isUndo = mod && !event.shiftKey && (event.key === 'z' || event.key === 'Z');
    if (!isUndo) return;
    if ((event.target as HTMLElement | null)?.isContentEditable) return;
    if (this.editHistory().length === 0) return;
    event.preventDefault();
    this.undoEdit();
  }

  /** Record the current free edits back into the dataset as evidence. Only the
   *  summary and role bullets are free prose we can trace to a dataset item;
   *  skill chips are skipped (they aren't free prose). Deterministic — no LLM.
   *  The PUT is validate-gated exactly like {@link onAccept}: a 422 surfaces the
   *  findings and leaves the edits intact so nothing is silently lost. */
  protected async recordEdits(): Promise<void> {
    const editMap = this.edits();
    // Only pending edits are recordable — keys already recorded this session stay
    // in `edits` for the preview but must not be re-written (they match editCount).
    const recordedSet = this.recordedKeys();
    const entries = Object.keys(editMap)
      .filter((k) => !recordedSet.has(k))
      .map((k) => ({ key: k, text: editMap[k] }));
    if (entries.length === 0) return;

    this.recording.set(true);
    try {
      const res = await this.recordLines(entries);
      if (!res.saved) {
        this.showToast(
          res.skippedSkills.length > 0
            ? 'Only summary and bullet edits can be recorded yet — skill edits stay local.'
            : 'Nothing to record — these edits don’t map to a dataset item.',
        );
        return;
      }
      // Never-fabricate honesty: a recorded line may still not trace (e.g. the
      // edit diverged from every evidence phrase). Warn, but never block the save.
      const provMap = provenanceIndex(this.provReport());
      const stillUnrecorded = res.recorded.filter((k) => provMap.get(k)?.status === 'unrecorded');

      let msg =
        stillUnrecorded.length > 0
          ? `Recorded, but ${plural(stillUnrecorded.length, 'line')} no longer trace to your evidence — review them.`
          : `Recorded ${plural(res.recorded.length, 'edit')} to your dataset.`;
      if (res.skippedSkills.length > 0) {
        msg += ` ${plural(res.skippedSkills.length, 'skill edit')} skipped — skills aren’t recordable yet.`;
      }
      this.showToast(msg);
    } catch (err) {
      this.showToast(this.saveErrorMessage(err));
    } finally {
      this.recording.set(false);
    }
  }

  /** Save the current pending edits INTO this stored build. Unlike
   *  {@link recordEdits} (which writes them to the DATASET as future evidence),
   *  this bakes them into THIS build's canonical draft: the server applies each
   *  under the never-fabricate guards, re-renders both PDFs, and appends them to
   *  the build's on-disk edit log. Only summary and bullet edits are savable —
   *  skill chips aren't traceable canonical lines, so they're excluded and noted.
   *  Targets are canonical ids (`summary`, `bullet:<source_id>`), never the
   *  preview's positional keys. On success the saved keys are cleared (they live
   *  in the build now) and the bundle is reloaded so preview / pixel-perfect /
   *  provenance all reflect the new stored truth. A 422 claim-divergence (or any
   *  other failure) surfaces via {@link errMessage}. */
  protected async saveEdits(): Promise<void> {
    const editMap = this.edits();
    const recordedSet = this.recordedKeys();
    const payload = this.previewPayload();
    // Same set the button counts: pending (not already recorded to the dataset).
    const pendingKeys = Object.keys(editMap).filter((k) => !recordedSet.has(k));
    if (pendingKeys.length === 0) return;

    // Translate positional preview keys → canonical targets the server accepts.
    const targets: Array<{ key: string; target: string; text: string }> = [];
    let skippedSkills = 0;
    for (const key of pendingKeys) {
      if (key === 'summary') {
        targets.push({ key, target: 'summary', text: editMap[key] });
      } else if (key.startsWith('bullet:')) {
        const sourceId = resolveBulletSourceId(key, payload);
        if (sourceId) targets.push({ key, target: `bullet:${sourceId}`, text: editMap[key] });
      } else if (key.startsWith('skill:')) {
        skippedSkills += 1; // skills aren't canonical lines — never savable
      }
    }

    if (targets.length === 0) {
      this.showToast(
        skippedSkills > 0
          ? 'Only summary and bullet edits can be saved into a build — skill edits stay local.'
          : 'Nothing to save — these edits don’t map to a build line.',
      );
      return;
    }

    this.saving.set(true);
    try {
      const res = await firstValueFrom(
        this.api.saveBuildEdits(
          this.id(),
          targets.map((t) => ({ target: t.target, text: t.text })),
        ),
      );
      let msg = `Saved ${plural(res.saved, 'edit')} into build ${this.id()} — PDFs re-rendered.`;
      if (skippedSkills > 0) {
        msg += ` ${plural(skippedSkills, 'skill edit')} skipped — skills aren’t savable to a build.`;
      }
      this.showToast(msg);
      // Baked into the build now — drop the saved keys from the local overlay,
      // the recorded set, and the undo history so they can't double-apply, then
      // reload the changed artifacts.
      const savedKeys = new Set(targets.map((t) => t.key));
      this.edits.update((m) => {
        const next = { ...m };
        for (const k of savedKeys) delete next[k];
        return next;
      });
      this.recordedKeys.update((s) => {
        const next = new Set(s);
        for (const k of savedKeys) next.delete(k);
        return next;
      });
      this.editHistory.update((h) => h.filter((e) => !savedKeys.has(e.key)));
      this.reloadBundle();
    } catch (err) {
      this.showToast(errMessage(err));
    } finally {
      this.saving.set(false);
    }
  }

  /** Revert one stored edit by posting its INVERSE (`{target, text: prev}`) to
   *  the same endpoint — an honest audit trail: the revert is appended to the
   *  log rather than erasing history.
   *
   *  Pending (unsaved) local edits are BLOCKED first: the preview renders the
   *  local `edits` overlay ON TOP of the bundle, so a pending edit would keep
   *  showing its pre-revert text over the correctly-reverted disk. On success
   *  the whole overlay is dropped (any remaining keys were recorded to the
   *  DATASET, which a build revert never touches — their text is safe there)
   *  and the bundle reloaded, so preview / pixel-perfect / provenance / the
   *  Edit history (now carrying this revert's inverse entry) all show the new
   *  stored truth. */
  protected async revertEdit(entry: EditLogEntry): Promise<void> {
    if (this.editCount() > 0) {
      this.showToast('Undo or save your pending edits first (skill edits can only be undone), then revert.');
      return;
    }
    this.saving.set(true);
    try {
      await firstValueFrom(
        this.api.saveBuildEdits(this.id(), [{ target: entry.target, text: entry.prev }]),
      );
      // Drop the local overlay so the reloaded bundle is what the preview
      // shows — a leftover recorded-key overlay would mask the reverted line.
      this.edits.set({});
      this.recordedKeys.set(new Set());
      this.editHistory.set([]);
      this.showToast(`Reverted ${this.editTargetLabel(entry.target).toLowerCase()} — PDFs re-rendered.`);
      this.reloadBundle();
    } catch (err) {
      this.showToast(errMessage(err));
    } finally {
      this.saving.set(false);
    }
  }

  /** Re-fetch this build's artifact bundle after its stored files changed (a
   *  save or revert baked edits into the canonical and re-rendered the PDFs) so
   *  the preview, pixel-perfect PDF, provenance, and edit history reflect the new
   *  stored truth. Lighter than the initial load — the dataset is unchanged, so
   *  only the bundle is refetched and the deterministic checks re-run. */
  private reloadBundle(): void {
    this.api.getBuild(this.id()).subscribe({
      next: (d) => {
        // Discard any layout-copilot projection: previewPayload prefers
        // `refinedHuman` over the bundle, so leaving it set would mask the
        // reloaded stored truth (a just-saved edit visually vanishes, and
        // recompute would spuriously claim-flag the stale payload against the
        // edited canonical). It was a projection of a canonical that just
        // changed — semantically it must not outlive this reload.
        this.refinedHuman.set(null);
        this.bundle.set(d as BuildBundle);
        void this.recompute();
      },
      error: (err: unknown) => this.showToast(`Couldn’t reload the build: ${errMessage(err)}`),
    });
  }

  /** Template helpers for the edit-history disclosure. */
  protected editEntryTime(iso: string): string {
    return formatStamp(iso);
  }
  protected editTargetLabel(target: string): string {
    if (target === 'summary') return 'Summary';
    if (target.startsWith('bullet:')) return 'Bullet';
    return target;
  }
  protected truncate(text: string, max = 64): string {
    return text.length > max ? `${text.slice(0, max - 1)}…` : text;
  }

  /** Confirm a single unrecorded line as the user's own evidence, straight from
   *  the provenance popover — no prior edit needed. Records the CANONICAL
   *  claim's text (from the provenance report line), NOT the preview's text:
   *  the preview shows the human variant's reword, but provenance checks the
   *  canonical draft — recording the reworded presentation would leave the flag
   *  raised forever. Same validated path as {@link recordEdits}; on success the
   *  line re-checks verbatim and the flag count drops. */
  protected async onConfirmLine(e: { key: string; text: string }): Promise<void> {
    // The claim being confirmed is the canonical line the checker flagged.
    const canonicalText = (this.provReport()?.lines ?? []).find((l) => keyOf(l) === e.key)?.text;
    try {
      const res = await this.recordLines([{ key: e.key, text: canonicalText ?? e.text }]);
      if (!res.saved) {
        this.showToast(
          res.skippedSkills.length > 0
            ? 'Skill lines aren’t recordable yet — edit it in the preview instead.'
            : 'Couldn’t record this line — it doesn’t map to a dataset item.',
        );
        return;
      }
      this.showToast('Confirmed — recorded as your evidence.');
    } catch (err) {
      this.showToast(this.saveErrorMessage(err));
    }
  }

  /** The single record path shared by the bulk "Record N edits" button and the
   *  per-line popover confirm. Maps each key to its dataset item (summary → the
   *  summary; `bullet:role:i` → the payload bullet's `source_id` → the dataset
   *  bullet) and writes the given CURRENT text; skill chips aren't free prose we
   *  can trace, so they're reported skipped. PUTs the enriched dataset (validate-
   *  gated — the caller catches a 422 and surfaces findings), then on success
   *  keeps the keys IN `edits` but marks them recorded (so they drop out of the
   *  pending `editCount`) and re-runs the deterministic checks. Throws on the PUT
   *  so callers own the toast copy. */
  private async recordLines(
    entries: { key: string; text: string }[],
  ): Promise<{ recorded: string[]; skippedSkills: string[]; unresolved: string[]; saved: boolean }> {
    const ds = this.dataset();
    const payload = this.previewPayload();
    const recorded: string[] = []; // keys we actually wrote into `next`
    const skippedSkills: string[] = [];
    const unresolved: string[] = [];
    if (!ds) return { recorded, skippedSkills, unresolved, saved: false };

    const next = structuredClone(ds) as ResumeDataset;
    for (const { key, text } of entries) {
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
      return { recorded, skippedSkills, unresolved, saved: false };
    }

    // The PUT responds with an ack ({status:"saved"}), not the dataset — keep the
    // object we sent, or every later check parses the ack as a dataset and wedges.
    await firstValueFrom(this.api.putDataset(next));
    this.dataset.set(next);
    // Keep the recorded keys IN `edits` so any edited preview text persists (it no
    // longer snaps back to the payload's pre-edit prose); mark them recorded so
    // they drop out of the pending `editCount`. A later re-edit re-pends the key
    // (see `onEdit`). A confirm of an un-edited line adds a key not in `edits` —
    // harmless, since `editCount` only counts `edits` keys.
    this.recordedKeys.update((s) => recorded.reduce((acc, k) => withAdded(acc, k), s));
    // Re-run the deterministic checks against the enriched dataset.
    await this.recompute();
    return { recorded, skippedSkills, unresolved, saved: true };
  }

  /** The shared save-error message: a 422 surfaces the validation findings, every
   *  other failure its plain message. */
  private saveErrorMessage(err: unknown): string {
    return err instanceof HttpErrorResponse && err.status === 422
      ? `Validation blocked the save: ${findings(err)}`
      : errMessage(err);
  }

  // ── claim check ──────────────────────────────────────────────────────
  /** The badge does double duty: it re-runs the deterministic check, and — when
   *  the draft is flagged — jumps the preview to the next unrecorded line so the
   *  count points at something actionable instead of nothing. */
  protected runClaimCheck(): void {
    if (this.claimState() === 'flag') this.jumpToNextFlagged();
    this.claimChecking.set(true);
    void this.recompute().finally(() => this.claimChecking.set(false));
  }

  /** Spotlight the next flagged line, cycling through them on repeated clicks. */
  private jumpToNextFlagged(): void {
    const keys = this.unrecordedKeys();
    if (keys.length === 0) return;
    if (this.cycleIndex >= keys.length) this.cycleIndex = 0;
    this.highlightKey.set(keys[this.cycleIndex]);
    this.cycleIndex = (this.cycleIndex + 1) % keys.length;
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
        // an edit onto the wrong dataset bullet. Drop pending edits on the swap —
        // and the undo stack too (its stale `prev` values would corrupt undo).
        this.edits.set({});
        this.editHistory.set([]);
        this.recordedKeys.set(new Set());
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
    if (r.aborted) {
      await this.applyAbortedRefine(r);
      return;
    }
    const outcome = refineOutcome(o.copilot, r);
    if (!outcome || !r.dataset) {
      this.showToast('No changes recorded.');
      return;
    }
    // Only mark the objection refined when actual evidence was recorded — a
    // declined-only outcome persists the declines but leaves the objection open.
    await this.persistEnrichedDataset(r.dataset, outcome.summary, outcome.recorded ? [o.id] : []);
  }

  /** The user ended a copilot session early. Keep whatever they recorded: if the
   *  returned (partially-enriched) dataset differs from the current one, PUT it
   *  via the validated path — but NEVER mark the objection refined, since the
   *  session didn't complete. Toast honestly either way. */
  private async applyAbortedRefine(r: RefineResult): Promise<void> {
    // `mutated` is decided wasm-side (one serializer, same map instance) — a
    // JS stringify-compare against our GET copy is unreliable because the two
    // processes serialize HashMap fields (skills.aliases) in different orders,
    // which read as "always changed".
    const changed = r.mutated === true;
    if (changed && r.dataset) {
      // markIds empty: kept the evidence, but the objection is NOT refined.
      await this.persistEnrichedDataset(
        r.dataset,
        'Session ended — kept what you recorded so far.',
        [],
      );
    } else {
      this.showToast('Session ended — nothing recorded.');
    }
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
      // Ack response — keep the dataset we sent (see recordEdits).
      await firstValueFrom(this.api.putDataset(dataset));
      this.dataset.set(dataset);
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
      if (r.aborted) {
        await this.applyAbortedRefine(r);
        return;
      }
      const outcome = refineOutcome('skills', r);
      if (!outcome || !r.dataset) {
        this.showToast(`Nothing recorded for “${name}”.`);
        return;
      }
      // Mark any open skills objection that names this requirement as refined —
      // but only when evidence was actually recorded (declined-only leaves them open).
      const needle = name.toLowerCase();
      const ids = outcome.recorded
        ? this.objectionVMs()
            .filter((o) => o.copilot === 'skills' && o.targetLabel.toLowerCase().includes(needle))
            .map((o) => o.id)
        : [];
      await this.persistEnrichedDataset(r.dataset, outcome.summary, ids);
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
        // Only objections a copilot can actually act on — the coverage map must
        // not route around the drawer's runnable gate into a guaranteed no-op.
        o.runnable &&
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
      // Ack response — keep the dataset we sent (see recordEdits).
      next: () => {
        this.dataset.set(next);
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
    // Download the edits-applied projection so the file matches the preview
    // exactly (falls back to the un-edited payload when there are no edits).
    const payload = this.effectivePayload();
    this.downloading.set(true);
    const done = () => this.downloading.set(false);
    if (payload) {
      // Render the variant actually being previewed (human when present, else
      // the ATS payload) with the template the picker has chosen — which
      // defaults to the payload's OWN stamp (bare name), never meta.template
      // (that records the ATS template id and 400s a human render). Falls back
      // to the stamp if the picker never loaded. The bare name is what the
      // resolver takes (the server also accepts the prefixed form).
      const templateName =
        this.chosenTemplate() || payload.template?.replace(/^(ats|human)\//, '') || undefined;
      this.api.render(payload.variant, payload, templateName).subscribe({
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
  /** Whether the interview actually changed the dataset — computed wasm-side
   *  (a JS compare against the GET copy is serialization-order unreliable). */
  mutated?: boolean;
  changed?: boolean;
  added?: number;
  verified?: number;
  removed?: number;
  skipped?: number;
  bullets_added?: number;
  declined?: number;
  /** The user ended the session early (dismissed a choice modal). `dataset`, if
   *  present, is the PARTIALLY-enriched dataset — their already-recorded answers,
   *  which must be kept. Never mark the objection refined on an abort. */
  aborted?: boolean;
  message?: string;
}

/** Cap on the session undo stack — enough for a real editing session without
 *  growing unbounded. */
const EDIT_HISTORY_CAP = 50;

const plural = (n: number, word: string): string => `${n} ${word}${n === 1 ? '' : 's'}`;

/** The persistable result of a dataset-enriching copilot: a plain summary to
 *  toast, plus whether any *evidence* was recorded — which is what marks the
 *  objection refined. A declined-only skills session is persistable (the declines
 *  are saved so they aren't re-offered) but records no evidence, so `recorded`
 *  is false and the objection stays open. Null = the user skipped everything. */
interface RefineOutcome {
  summary: string;
  recorded: boolean;
}

function refineOutcome(kind: CopilotKind, r: RefineResult): RefineOutcome | null {
  switch (kind) {
    case 'strengthen':
      return r.changed
        ? { summary: 'Recorded a stronger line as evidence — it lands on your next build.', recorded: true }
        : null;
    case 'summary':
      return r.changed ? { summary: 'Updated summary saved to your dataset.', recorded: true } : null;
    case 'metric':
      return (r.added ?? 0) > 0
        ? { summary: `Recorded ${plural(r.added ?? 0, 'metric')} as evidence.`, recorded: true }
        : null;
    case 'skills': {
      const parts: string[] = [];
      if (r.verified) parts.push(`verified ${plural(r.verified, 'skill')}`);
      if (r.bullets_added) parts.push(`added ${plural(r.bullets_added, 'bullet')}`);
      if (r.removed) parts.push(`removed ${plural(r.removed, 'skill')}`);
      const recorded = parts.length > 0;
      const declined = r.declined ?? 0;
      // Declined-only: nothing was recorded as evidence, but the declines ARE
      // saved (so they won't be offered again) — a persistable, non-refining
      // outcome with its own honest message.
      if (!recorded) {
        return declined > 0
          ? {
              summary: `Noted — ${plural(declined, 'keyword')} declined; they won’t be offered again.`,
              recorded: false,
            }
          : null;
      }
      // Real evidence, optionally alongside declines ("Verified 1 skill · 2 declined").
      if (declined > 0) parts.push(`${declined} declined`);
      const s = parts.join(' · ');
      return { summary: `${s.charAt(0).toUpperCase()}${s.slice(1)}.`, recorded };
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
