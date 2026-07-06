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
} from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { DomSanitizer, type SafeResourceUrl } from '@angular/platform-browser';
import { HttpErrorResponse } from '@angular/common/http';

import { ApiService } from '../../services/api.service';
import { WasmService } from '../../services/wasm.service';
import { CopilotHost } from '../../shared/copilot-host';
import { triggerDownload } from '../../shared/download';
import type { CoverBrief, CoverLetter, JobRequirements, TailoredResume } from '../../models';

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
  template: `
    @if (!canonicalPresent()) {
      <div class="panel muted">
        <b>No tailored résumé yet.</b>
        <p>A cover letter is drawn from this build's tailored résumé and job description. Tailor a résumé for this build first, then come back to draft the letter.</p>
      </div>
    } @else if (hasContent()) {
      <div class="cover-head">
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

    .cover-head { display: flex; align-items: center; justify-content: flex-end; margin: 4px 0 14px; }
    .cover-actions { display: inline-flex; gap: 10px; }

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
  /** Ask the workspace to reload the build bundle after a (re)generate, so the
   *  new cover joins the build's `pdfs` list and the chat context. */
  readonly generated = output<void>();
  /** Surface a message through the workspace toast (generation errors). */
  readonly notify = output<string>();

  protected readonly generating = signal(false);
  protected readonly errorMsg = signal<string | null>(null);
  protected readonly warnings = signal<string[]>([]);
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
    // bleed across builds.
    effect(() => {
      this.buildId();
      this.letter.set(null);
      this.warnings.set([]);
      this.errorMsg.set(null);
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
