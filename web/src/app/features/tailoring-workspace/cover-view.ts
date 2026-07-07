import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  untracked,
} from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { DomSanitizer, type SafeResourceUrl } from '@angular/platform-browser';
import { HttpErrorResponse } from '@angular/common/http';

import { ApiService } from '../../services/api.service';
import { WasmService } from '../../services/wasm.service';
import { CopilotHost } from '../../shared/copilot-host';
import { triggerDownload } from '../../shared/download';
import type {
  CoverBrief,
  CoverLetter,
  CoverProvenanceReport,
  JobRequirements,
  TailoredResume,
} from '../../models';
import { CoverPreview } from './cover-preview';
import { coverBadgeText } from './cover-preview.model';

/** The filename the cover render is persisted under, both in the build's `pdfs`
 *  list and at `GET /api/builds/:id/files/cover_letter.pdf`. */
export const COVER_PDF = 'cover_letter.pdf';

/** Whether a build's `pdfs` list already carries a rendered cover letter — the
 *  test that flips the Cover Letter view between its Generate landing and its
 *  PDF preview. */
export function coverExists(pdfs: readonly string[] | undefined): boolean {
  return (pdfs ?? []).includes(COVER_PDF);
}

/** Whether a `CoverBrief` carries nothing at all — every scalar blank, both
 *  lists empty. An interview the user cancelled before answering anything (or
 *  a completed one where every slot was skipped/declined) yields exactly this
 *  shape; a brief with even one answer does not. Drives whether "Draft with
 *  copilot" bothers generating at all once the interview resolves: nothing
 *  gathered reads as "the person backed out", not "draft with an empty brief". */
export function isEmptyBrief(brief: CoverBrief | null | undefined): boolean {
  if (!brief) return true;
  return (
    !brief.angle?.trim() &&
    brief.emphasis.length === 0 &&
    !brief.tone?.trim() &&
    !brief.motivation?.trim() &&
    brief.constraints.length === 0
  );
}

/** The Cover Letter view: the third workspace pill. A build with no cover shows
 *  a short explainer and a Generate button; a build that has one renders its
 *  rendered `cover_letter.pdf` in an iframe with Download and Regenerate.
 *
 *  Honesty: the letter is drafted from the build's tailored résumé and JD (the
 *  same evidence-traced facts), so the copy never implies the model invents
 *  experience. Generation is a real LLM call, so the button shows a busy state
 *  and any server error surfaces inline (and to the workspace toast).
 *
 *  Blob lifecycle mirrors {@link PdfPreview}: the fetched PDF becomes an object
 *  URL, revoked when the build changes, when a fresh render replaces it, and on
 *  destroy — so no object URL outlives the DOM that points at it. */
@Component({
  selector: 'app-cover-view',
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [CoverPreview],
  template: `
    @if (!canonicalPresent()) {
      <div class="panel muted">
        <b>No tailored résumé yet.</b>
        <p>A cover letter is drawn from this build's tailored résumé and job description. Tailor a résumé for this build first, then come back to draft the letter.</p>
      </div>
    } @else if (hasContent()) {
      <div class="cover-head">
        <div class="cover-modes">
          <div class="segmented" role="tablist" aria-label="Cover letter fidelity">
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
          @if (coverReport()) {
            <div class="cc-wrap">
              <span class="claimcheck" [attr.data-state]="coverClaimState()">
                <span class="cc-dot" aria-hidden="true"></span>
                <span>{{ coverClaimText() }}</span>
              </span>
              <button
                class="cc-info"
                type="button"
                aria-label="What the paragraph check verifies"
                (mouseenter)="infoOpen.set(true)"
                (focus)="infoOpen.set(true)"
                (mouseleave)="infoOpen.set(false)"
                (blur)="infoOpen.set(false)"
              >i</button>
              @if (infoOpen()) {
                <div class="cc-tip" role="tooltip">
                  Each paragraph is checked against your evidence:
                  <b>traced</b> means every fact comes from your resume or the posting;
                  <b>needs a look</b> means it mentions something neither carries;
                  <b>connecting language</b> makes no specific claim to check.
                  This is informational, and these edits stay local until you Regenerate.
                </div>
              }
            </div>
          }
        </div>
        <div class="cover-actions">
          <button class="btn" type="button" (click)="download()" [disabled]="!url()">Download PDF</button>
          <button class="btn" type="button" (click)="generate()" [disabled]="actionsDisabled()">
            {{ generating() ? 'Regenerating…' : 'Regenerate' }}
          </button>
          <button class="btn" type="button" (click)="draftWithCopilot()" [disabled]="actionsDisabled()">
            Draft with copilot
          </button>
        </div>
      </div>

      @if (errorMsg(); as e) {
        <div class="cover-error" role="alert">{{ e }}</div>
      }
      @if (warnings().length > 0) {
        <div class="cover-warn" role="status">
          @for (w of warnings(); track w) {
            <div>{{ w }}</div>
          }
        </div>
      }

      @if (previewMode() === 'pixel') {
        <div class="pdfwrap">
          @if (url(); as u) {
            <iframe class="pdf" [src]="u" title="Rendered cover letter PDF"></iframe>
          }
          @if (loadingPdf()) {
            <div class="pv-status" role="status"><span class="spin" aria-hidden="true"></span> Loading…</div>
          } @else if (!url()) {
            <div class="pv-status muted">Couldn't load the cover letter PDF.</div>
          }
        </div>

        @if (letter()?.paragraphs?.length) {
          <details class="cover-text">
            <summary>Letter text</summary>
            @for (p of letter()!.paragraphs; track $index) {
              <p>{{ p }}</p>
            }
          </details>
        }
      } @else {
        @if (coverPayload(); as cp) {
          @if (coverReport(); as rep) {
            <app-cover-preview
              [report]="rep"
              [greeting]="cp.greeting"
              [signoff]="cp.signoff"
              (edit)="onParaEdit($event)"
              (confirm)="onConfirmParagraph($event)"
            />
          } @else {
            <div class="panel muted">Checking the letter's paragraphs…</div>
          }
        } @else {
          <div class="panel muted">
            <b>No paragraph data for this letter yet.</b>
            <p>This build's cover letter was drafted before per-paragraph editing existed. Regenerate it, or switch to Pixel-perfect to view the rendered PDF.</p>
          </div>
        }
      }
    } @else {
      <div class="panel">
        <b>Draft a cover letter for this job.</b>
        <p class="muted">It is written from your tailored résumé and this posting, using your recorded voice samples for tone. It won't claim experience you haven't recorded.</p>
        @if (errorMsg(); as e) {
          <div class="cover-error" role="alert">{{ e }}</div>
        }
        <div class="cover-actions">
          <button class="btn btn-primary" type="button" (click)="generate()" [disabled]="actionsDisabled()">
            {{ generating() ? 'Generating…' : 'Generate cover letter' }}
          </button>
          <button class="btn" type="button" (click)="draftWithCopilot()" [disabled]="actionsDisabled()">
            Draft with copilot
          </button>
        </div>
        @if (generating()) {
          <span class="gen-note">This is a live model call and takes a few seconds.</span>
        }
      </div>
    }
  `,
  styles: `
    :host { display: block; }
    .panel { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 22px; max-width: 62ch; display: flex; flex-direction: column; gap: 12px; align-items: flex-start; }
    .panel p { margin: 0; line-height: 1.55; }
    .muted { color: var(--muted); }

    .cover-head { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; margin: 4px 0 14px; }
    .cover-actions { display: inline-flex; gap: 10px; }
    .cover-modes { display: inline-flex; align-items: center; gap: 12px; flex-wrap: wrap; }

    .segmented { display: inline-flex; padding: 3px; gap: 3px; background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px; }
    .segmented button { display: inline-flex; align-items: center; padding: 6px 14px; border: 0; border-radius: 999px; background: transparent; font: inherit; font-size: 13px; font-weight: 500; color: var(--muted); cursor: pointer; transition: background 0.15s, color 0.15s, box-shadow 0.15s; }
    .segmented button.on { background: var(--surface); color: var(--fg); box-shadow: 0 1px 2px color-mix(in oklch, var(--fg) 12%, transparent); }
    .segmented button:not(.on):hover { color: var(--fg); }
    .segmented button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }

    .cc-wrap { position: relative; display: inline-flex; align-items: center; gap: 6px; }
    .claimcheck { display: inline-flex; align-items: center; gap: 9px; padding: 7px 13px; border-radius: 999px; font-family: var(--font-mono); font-size: 12px; border: 1px solid; background: var(--surface-2); color: var(--muted); }
    .claimcheck .cc-dot { width: 9px; height: 9px; border-radius: 50%; background: var(--muted); }
    .claimcheck[data-state='ok'] { color: var(--success); border-color: color-mix(in oklch, var(--success) 38%, var(--border)); background: var(--success-bg); }
    .claimcheck[data-state='ok'] .cc-dot { background: var(--success); }
    .claimcheck[data-state='flag'] { color: var(--warn); border-color: color-mix(in oklch, var(--warn) 45%, var(--border)); background: var(--warn-bg); }
    .claimcheck[data-state='flag'] .cc-dot { background: var(--warn); }
    .cc-info { width: 20px; height: 20px; border-radius: 50%; border: 1px solid var(--border); background: var(--surface); color: var(--muted); font-family: var(--font-mono); font-size: 11px; line-height: 1; cursor: help; }
    .cc-info:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .cc-tip { position: absolute; top: 30px; left: 0; z-index: 20; width: 320px; background: var(--fg); color: oklch(96% 0.01 80); padding: 12px 14px; border-radius: 9px; font-size: 12.5px; line-height: 1.5; box-shadow: 0 12px 30px -12px color-mix(in oklch, var(--fg) 60%, transparent); }
    .cc-tip b { color: oklch(99% 0.01 80); }

    .btn { display: inline-flex; align-items: center; gap: 8px; height: 34px; padding: 0 14px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 14px; font-weight: 500; color: inherit; cursor: pointer; }
    .btn:hover:not(:disabled) { border-color: var(--fg); }
    .btn:disabled { opacity: 0.6; cursor: default; }
    .btn-primary { background: var(--accent); color: oklch(97% 0.02 40); border-color: var(--accent); }
    .btn-primary:hover:not(:disabled) { background: var(--accent-2); border-color: var(--accent-2); }
    .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }

    .gen-note { font-size: 12.5px; color: var(--faint); }

    .cover-error { max-width: 62ch; border: 1px solid color-mix(in oklch, var(--danger) 45%, var(--border)); background: color-mix(in oklch, var(--danger) 8%, var(--surface)); color: var(--fg); border-radius: var(--radius); padding: 11px 14px; margin-bottom: 14px; font-size: 13.5px; line-height: 1.5; }
    .cover-warn { max-width: 62ch; border: 1px solid color-mix(in oklch, var(--warn) 45%, var(--border)); background: var(--warn-bg); color: var(--fg); border-radius: var(--radius); padding: 11px 14px; margin-bottom: 14px; font-size: 12.5px; line-height: 1.5; display: flex; flex-direction: column; gap: 4px; }

    .pdfwrap { position: relative; }
    .pdf { display: block; width: 100%; height: 78vh; border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--surface); }
    .pv-status { display: inline-flex; align-items: center; gap: 8px; position: absolute; top: 12px; left: 12px; font-family: var(--font-mono); font-size: 12px; padding: 6px 11px; border-radius: 999px; border: 1px solid var(--border); background: var(--surface); color: var(--muted); box-shadow: 0 2px 8px -4px color-mix(in oklch, var(--fg) 30%, transparent); }
    .pv-status.muted { position: static; box-shadow: none; }

    .cover-text { margin-top: 16px; max-width: 68ch; }
    .cover-text summary { cursor: pointer; font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); }
    .cover-text p { margin: 12px 0 0; line-height: 1.6; font-size: 14.5px; }

    .spin { width: 12px; height: 12px; border-radius: 50%; border: 2px solid var(--border); border-top-color: var(--accent); animation: cv-spin 0.7s linear infinite; }
    @media (prefers-reduced-motion: reduce) { .spin { animation: none; } }
    @keyframes cv-spin { to { transform: rotate(360deg); } }
  `,
})
export class CoverView {
  private readonly api = inject(ApiService);
  private readonly sanitizer = inject(DomSanitizer);
  private readonly destroyRef = inject(DestroyRef);
  private readonly wasm = inject(WasmService);
  protected readonly copilot = inject(CopilotHost);

  readonly buildId = input.required<string>();
  /** Whether the build's `pdfs` already lists `cover_letter.pdf` (drives the
   *  initial auto-load of the stored PDF on open). */
  readonly hasCover = input.required<boolean>();
  /** Whether the build has a canonical draft — a letter can only be drawn from
   *  one, so an absent canonical shows an explanatory empty state, never a POST
   *  that would 404. */
  readonly canonicalPresent = input.required<boolean>();
  /** The build's JD, for the "Draft with copilot" interview to ground its
   *  questions in. Null until the workspace's bundle has loaded one. */
  readonly jd = input<JobRequirements | null>(null);
  /** The build's canonical tailored résumé, for the same interview. Null until
   *  loaded (mirrors {@link canonicalPresent}, which gates the whole view on
   *  this being non-null in practice). */
  readonly canonical = input<TailoredResume | null>(null);
  /** The build's drafted cover letter's parsed fields (`cover_payload.json`),
   *  when the bundle carries them — the source for the editing pane's
   *  per-paragraph provenance view. Null for a build with only a rendered PDF
   *  (a cover drafted before this field existed), in which case the editing
   *  pane shows an explanatory empty state and Pixel-perfect still works. */
  readonly coverPayload = input<CoverLetter | null>(null);
  /** The build's persisted cover-letter interview brief (`cover_brief.json`),
   *  when the bundle carries one — fed to `checkCoverProvenance` as grounding,
   *  the same corpus a fresh draft would read. Null for a build with no saved
   *  brief (never interviewed, or drafted before this field existed). */
  readonly coverBrief = input<CoverBrief | null>(null);
  /** Ask the workspace to reload the build bundle after a (re)generate, so the
   *  new cover joins the build's `pdfs` list and the chat context. */
  readonly generated = output<void>();
  /** Surface a message through the workspace toast (a generation error, or a
   *  confirm-as-evidence result). */
  readonly notify = output<string>();

  protected readonly generating = signal(false);
  protected readonly errorMsg = signal<string | null>(null);
  protected readonly warnings = signal<string[]>([]);

  /** The fidelity sub-toggle, mirroring the résumé preview: the provenance-aware
   *  editing pane (`editing`, the default, to surface where each paragraph
   *  traces) or the real Typst-rendered PDF (`pixel`). Reset per build. */
  protected readonly previewMode = signal<'editing' | 'pixel'>('editing');
  /** The letter's body paragraphs as a local, in-memory working copy — seeded
   *  from {@link coverPayload}, spliced on each edit. Never persisted; a fresh
   *  build reseeds it, a reload discards it. */
  private readonly paragraphs = signal<string[]>([]);
  /** The deterministic per-paragraph provenance verdict for the working copy —
   *  recomputed locally on every edit via `checkCoverProvenance`. Both the
   *  editing pane and the claim badge read it, so the badge count always matches
   *  what the paragraphs show. */
  protected readonly coverReport = signal<CoverProvenanceReport | null>(null);
  /** Whether the badge's explanatory tooltip is open. */
  protected readonly infoOpen = signal(false);
  /** A brief this session's own "confirm as evidence" action just persisted,
   *  overriding {@link coverBrief} until a fresh bundle load supersedes it (a
   *  build switch resets it). Kept separate from the {@link coverBrief} input
   *  so applying it never re-triggers the seeding effect below and discards
   *  an in-progress paragraph edit — only the explicit recheck calls below
   *  read it. */
  private readonly confirmedBrief = signal<CoverBrief | null>(null);

  /** How many body paragraphs still read as unrecorded — the badge's count and
   *  its flag/ok state. Informational only, never a gate. */
  protected readonly unrecordedCount = computed(
    () => (this.coverReport()?.paragraphs ?? []).filter((p) => p.status === 'unrecorded').length,
  );
  private readonly paraTotal = computed(() => this.coverReport()?.paragraphs.length ?? 0);
  protected readonly coverClaimText = computed(() =>
    coverBadgeText(this.unrecordedCount(), this.paraTotal()),
  );
  protected readonly coverClaimState = computed<'ok' | 'flag'>(() =>
    this.unrecordedCount() > 0 ? 'flag' : 'ok',
  );
  /** Monotonic id so a slow provenance check can't clobber a newer one (an edit
   *  landing while a prior recheck is still resolving). */
  private recheckSeq = 0;
  /** The most recently generated letter, so its paragraphs can be shown under
   *  the PDF. Null after a fresh open (a reload from disk carries only the PDF). */
  protected readonly letter = signal<CoverLetter | null>(null);
  protected readonly url = signal<SafeResourceUrl | null>(null);
  protected readonly loadingPdf = signal(false);

  /** Show the PDF area once there's a cover to show (on disk or just drafted). */
  protected readonly hasContent = computed(() => this.hasCover() || this.url() !== null);

  /** Every button on this view is disabled while a generate is in flight OR
   *  while the copilot interview (which runs through the same shared modal
   *  every other copilot uses) is running — the two must never overlap. */
  protected readonly actionsDisabled = computed(() => this.generating() || this.copilot.running() !== null);

  private blobUrl: string | null = null;
  private loadedFor: string | null = null;
  /** Monotonic fetch id so a superseded PDF load never clobbers a newer one. */
  private pdfReqId = 0;

  constructor() {
    this.destroyRef.onDestroy(() => this.revoke());
    // Auto-load the stored cover PDF for a build that already has one. Re-fetch
    // on a build switch; drop it when the build has no cover.
    effect(() => {
      const id = this.buildId();
      if (!this.hasCover()) {
        if (this.loadedFor !== null) {
          this.revoke();
          this.url.set(null);
          this.loadedFor = null;
        }
        return;
      }
      if (this.loadedFor === id) return; // already loaded for this build
      this.fetchPdf(id);
    });
    // A build switch resets the transient letter/warnings/error so they never
    // bleed across builds, and returns the fidelity toggle to its default. A
    // switch also drops any confirm-as-evidence override from the PRIOR build —
    // it must never leak its grounding into a different build's re-check.
    effect(() => {
      this.buildId();
      this.letter.set(null);
      this.warnings.set([]);
      this.errorMsg.set(null);
      this.previewMode.set('editing');
      this.infoOpen.set(false);
      this.confirmedBrief.set(null);
    });

    // Seed the editable paragraph copy from the build's cover payload and run
    // the initial provenance check. Re-runs when the payload, the résumé/JD
    // it's checked against, or the bundle's saved brief changes — a fresh
    // build, or those inputs arriving after the payload. Local edits go
    // through onParaEdit, not this effect, so a mid-session edit is never
    // clobbered here (the inputs are fixed per build); a session-local confirm
    // override is read only inside the recheck calls below, never here, so
    // confirming a paragraph never re-seeds (and so never discards) the
    // working paragraph copy.
    effect(() => {
      const letter = this.coverPayload();
      const resume = this.canonical();
      const jd = this.jd();
      const brief = this.coverBrief();
      const paras = letter?.paragraphs ? [...letter.paragraphs] : [];
      untracked(() => {
        this.paragraphs.set(paras);
        this.coverReport.set(null);
        this.confirmedBrief.set(null);
      });
      if (letter && resume && jd) void this.recheck(letter, resume, jd, paras, brief);
    });
  }

  /** The brief to check paragraphs against right now: a session-local
   *  "confirm as evidence" result if one has landed, else the bundle's own
   *  {@link coverBrief}. Read directly (never inside the seeding effect above)
   *  so applying a confirm never re-triggers that effect's paragraph reset. */
  private effectiveBrief(): CoverBrief | null {
    return this.confirmedBrief() ?? this.coverBrief();
  }

  /** Re-run the deterministic per-paragraph classifier for `paras` against
   *  `brief` and publish the result — the one place the report is produced,
   *  shared by the editing pane and the badge. Classification is independent
   *  per paragraph, so re-checking the whole (short) letter on each edit is
   *  both simplest and correct. A stale result (an older check resolving after
   *  a newer edit) is dropped via {@link recheckSeq}. */
  private async recheck(
    letter: CoverLetter,
    resume: TailoredResume,
    jd: JobRequirements,
    paras: string[],
    brief: CoverBrief | null,
  ): Promise<void> {
    const seq = ++this.recheckSeq;
    const draft: CoverLetter = { ...letter, paragraphs: paras };
    try {
      const report = await this.wasm.checkCoverProvenance(draft, resume, jd, brief);
      if (seq === this.recheckSeq) this.coverReport.set(report);
    } catch {
      if (seq === this.recheckSeq) this.coverReport.set(null);
    }
  }

  /** A paragraph was edited in the editing pane: splice it into the local
   *  working copy and re-run the classifier so its status (and the badge count)
   *  updates live. Purely in-memory — nothing is saved; Regenerate is the
   *  durable path. */
  protected onParaEdit(e: { index: number; text: string }): void {
    const letter = this.coverPayload();
    const resume = this.canonical();
    const jd = this.jd();
    if (!letter || !resume || !jd) return;
    const paras = [...this.paragraphs()];
    if (e.index < 0 || e.index >= paras.length || paras[e.index] === e.text) return;
    paras[e.index] = e.text;
    this.paragraphs.set(paras);
    void this.recheck(letter, resume, jd, paras, this.effectiveBrief());
  }

  /** "Confirm as evidence" on an `unrecorded` paragraph: persist its own text
   *  (verbatim — no model call here, see {@link CoverPreview.confirm}'s doc)
   *  as a new `CoverBrief.emphasis` entry, then re-check locally against the
   *  brief the server just saved, so the paragraph's status updates live
   *  without regenerating the letter. A failed request surfaces through the
   *  workspace toast, matching {@link runGenerate}'s error handling. */
  protected onConfirmParagraph(e: { index: number; text: string }): void {
    const text = e.text.trim();
    if (!text) return;
    const id = this.buildId();
    this.api
      .confirmCoverEvidence(id, text)
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (res) => {
          this.confirmedBrief.set(res.brief);
          const letter = this.coverPayload();
          const resume = this.canonical();
          const jd = this.jd();
          if (letter && resume && jd) {
            void this.recheck(letter, resume, jd, this.paragraphs(), res.brief);
          }
          this.notify.emit('Confirmed: added as evidence for future drafts.');
        },
        error: (err: unknown) => {
          this.notify.emit(`Couldn’t confirm this paragraph: ${coverErrorMessage(err)}`);
        },
      });
  }

  /** Draft (or redraft) the cover letter for the open build, then refresh the
   *  preview and ask the workspace to reload its bundle. */
  protected generate(): void {
    this.runGenerate(undefined);
  }

  /** Run the cover-letter interview (`cover_interview_interactive`) through the
   *  shared copilot modal, then draft with whatever it gathered.
   *
   *  Cancellation design: the interview degrades to a partial (or entirely
   *  empty) brief rather than erroring on a declined suggestion menu or a
   *  skipped question — see `cover_interview_interactive`'s doc comment. So
   *  there is no separate "aborted" signal to branch on here: if the brief
   *  that comes back is completely empty (the person backed out before
   *  answering anything), this does nothing — the same as never having
   *  clicked the button. If it carries even one answer, the letter is drafted
   *  from exactly that — never all-or-nothing, and never generating from an
   *  interview the person visibly abandoned before saying anything. */
  protected async draftWithCopilot(): Promise<void> {
    if (this.actionsDisabled()) return;
    const jd = this.jd();
    const canonical = this.canonical();
    if (!jd || !canonical) return; // same guard `canonicalPresent` already implies
    this.errorMsg.set(null);
    try {
      const brief = await this.copilot.runWithUi('cover letter copilot', () =>
        this.wasm.coverInterview(canonical, jd),
      );
      if (isEmptyBrief(brief)) return; // nothing gathered — treat as a clean cancel
      this.runGenerate(brief);
    } catch (err) {
      const msg = coverErrorMessage(err);
      this.errorMsg.set(msg);
      this.notify.emit(msg);
    }
  }

  /** Shared by {@link generate} and {@link draftWithCopilot}: draft (or
   *  redraft) the cover letter, optionally grounded in an interview's
   *  `CoverBrief`, then refresh the preview and ask the workspace to reload
   *  its bundle. */
  private runGenerate(brief: CoverBrief | undefined): void {
    if (this.generating() || !this.canonicalPresent()) return;
    const id = this.buildId();
    this.generating.set(true);
    this.errorMsg.set(null);
    this.api
      .generateCover(id, brief)
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (res) => {
          this.generating.set(false);
          this.letter.set(res.letter);
          this.warnings.set(res.warnings ?? []);
          this.fetchPdf(id); // the render just changed on disk — pull it fresh
          this.generated.emit();
        },
        error: (err: unknown) => {
          this.generating.set(false);
          const msg = coverErrorMessage(err);
          this.errorMsg.set(msg);
          this.notify.emit(msg);
        },
      });
  }

  protected download(): void {
    const id = this.buildId();
    this.api
      .getBuildFile(id, COVER_PDF)
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (blob) => triggerDownload(blob, COVER_PDF),
        error: (err: unknown) => this.notify.emit(`Download failed: ${coverErrorMessage(err)}`),
      });
  }

  private fetchPdf(id: string): void {
    const req = ++this.pdfReqId;
    this.loadingPdf.set(true);
    this.api
      .getBuildFile(id, COVER_PDF)
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (blob) => {
          if (req !== this.pdfReqId) return; // superseded
          this.revoke();
          this.blobUrl = URL.createObjectURL(blob);
          this.url.set(
            this.sanitizer.bypassSecurityTrustResourceUrl(
              `${this.blobUrl}#toolbar=0&navpanes=0&view=FitH`,
            ),
          );
          this.loadedFor = id;
          this.loadingPdf.set(false);
        },
        error: () => {
          if (req !== this.pdfReqId) return;
          this.loadingPdf.set(false);
        },
      });
  }

  private revoke(): void {
    if (this.blobUrl) {
      URL.revokeObjectURL(this.blobUrl);
      this.blobUrl = null;
    }
  }
}

/** The human message behind a failed cover request — the server envelope's
 *  message, a plain string, or an `Error`. Mirrors the workspace's own unwrap so
 *  a failed draft reads the same clean line the other actions do. */
function coverErrorMessage(err: unknown): string {
  if (err instanceof HttpErrorResponse) {
    const b = err.error as { error?: { message?: string }; message?: string } | string | null;
    if (typeof b === 'string') return b || err.message;
    return b?.error?.message ?? b?.message ?? err.message;
  }
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  return 'the request failed';
}
