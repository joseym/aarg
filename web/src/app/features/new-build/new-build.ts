import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { firstValueFrom } from 'rxjs';

import { ApiService } from '../../services/api.service';
import { WasmService } from '../../services/wasm.service';
import { CopilotHost } from '../../shared/copilot-host';
import { BuildRunner } from '../../services/build-runner';
import { BuildsStore } from '../../services/builds-store';
import type { JobRequirements } from '../../models';

/** Which of the three mutually-exclusive JD sources is active. */
type JdSource = 'paste' | 'url' | 'reuse';

/** `/new` — the New-Build screen. Pick a job description one of three ways —
 *  paste it, fetch it from a URL, or reuse a previous build's parsed JD — then
 *  run the full adversarial tailor loop entirely in the browser via the shared
 *  {@link BuildRunner}: analyze the gap → drive the capped tailor/review/revise
 *  loop → project the human variant, and persist the result via `POST /api/builds`.
 *  The live iteration/score/cost overlay is the shared {@link CopilotHost}
 *  progress panel — {@link BuildRunner} runs the loop inside `runWithUi`, which
 *  lights it up for free.
 *
 *  Reusing a previous JD is a *retailor*: the same posting, re-run against your
 *  current (possibly copilot-enriched) dataset — the one code path New Build and
 *  the per-build Retailor button both share.
 *
 *  This is the same loop `aarg tailor` runs, live. It can be stopped between
 *  passes via the Stop button in the progress overlay (an in-flight model call
 *  still completes); the form stays disabled until the run settles (success,
 *  stop, or error). */
@Component({
  selector: 'app-new-build',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="wrap">
      <header class="head">
        <div class="kicker"><span class="dot"></span> New build</div>
        <h1>Tailor a résumé to a job description</h1>
        <p class="sub">
          Paste the posting (or fetch it from a URL), then run the adversarial
          tailor loop. It parses the JD, finds the gaps against your dataset, and
          drafts → reviews → revises a canonical résumé: the same loop the CLI
          runs, live in your browser.
        </p>
      </header>

      <!-- ── JD source ── -->
      <section class="field">
        <label class="lbl">Job description</label>

        <!-- three mutually-exclusive sources -->
        <div class="seg" role="tablist" aria-label="Job description source">
          <button
            class="seg-btn"
            type="button"
            role="tab"
            [attr.aria-selected]="source() === 'paste'"
            [class.on]="source() === 'paste'"
            (click)="source.set('paste')"
            [disabled]="running()"
          >
            Paste
          </button>
          <button
            class="seg-btn"
            type="button"
            role="tab"
            [attr.aria-selected]="source() === 'url'"
            [class.on]="source() === 'url'"
            (click)="source.set('url')"
            [disabled]="running()"
          >
            URL
          </button>
          <button
            class="seg-btn"
            type="button"
            role="tab"
            [attr.aria-selected]="source() === 'reuse'"
            [class.on]="source() === 'reuse'"
            (click)="source.set('reuse')"
            [disabled]="running()"
          >
            Reuse a previous build
          </button>
        </div>

        @if (source() === 'url') {
          <div class="url-row">
            <input
              id="jd-url"
              class="in"
              type="url"
              inputmode="url"
              placeholder="https://… paste a posting URL to fetch"
              [value]="url()"
              (input)="url.set(asValue($event))"
              [disabled]="running()"
            />
            <button
              class="btn"
              type="button"
              (click)="fetchJd()"
              [disabled]="running() || fetching() || !url().trim()"
            >
              {{ fetching() ? 'Fetching…' : 'Fetch' }}
            </button>
          </div>
        }

        @if (source() === 'reuse') {
          @if (builds().length > 0) {
            <select
              class="in reuse"
              aria-label="Reuse a previous build's job description"
              [value]="selectedBuildId()"
              (change)="selectedBuildId.set(asValue($event))"
              [disabled]="running()"
            >
              <option value="" disabled>Pick a previous build to retailor…</option>
              @for (b of builds(); track b.id) {
                <option [value]="b.id">{{ b.target }}</option>
              }
            </select>
            <p class="note">
              Retailors the chosen posting against your current, possibly
              copilot-enriched dataset: the same loop, fresh evidence.
            </p>
          } @else {
            <p class="note">No previous builds yet: paste or fetch a posting instead.</p>
          }
        } @else {
          <textarea
            id="jd"
            class="ta"
            rows="12"
            placeholder="…paste the full job description here."
            [value]="jdText()"
            (input)="jdText.set(asValue($event))"
            [disabled]="running()"
          ></textarea>
        }
      </section>

      <div class="actions">
        <button
          class="btn btn-primary"
          type="button"
          (click)="runBuild()"
          [disabled]="running() || !canRun()"
        >
          {{ running() ? 'Running…' : source() === 'reuse' ? 'Retailor' : 'Run build' }}
        </button>
        <p class="note">
          Runs live: you can Stop it between passes from the progress overlay
          (an in-flight model call still finishes). Give it a minute.
        </p>
      </div>
    </div>

    @if (toast(); as t) {
      <div class="toast on" role="status">{{ t }}</div>
    }
  `,
  styles: `
    :host { display: block; }
    .wrap { max-width: 760px; display: flex; flex-direction: column; gap: 24px; }

    .head { display: flex; flex-direction: column; gap: 8px; }
    .kicker { font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--accent); display: flex; align-items: center; gap: 10px; }
    .kicker .dot { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); }
    .head h1 { font-family: var(--font-display); font-size: 26px; line-height: 1.15; margin: 4px 0 0; }
    .sub { color: var(--muted); font-size: 14.5px; line-height: 1.55; margin: 0; max-width: 60ch; }

    .field { display: flex; flex-direction: column; gap: 10px; }
    .lbl { font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); }
    .url-row { display: flex; gap: 10px; }
    .url-row .in { flex: 1; }

    .in { height: 38px; padding: 0 12px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); color: var(--fg); font: inherit; font-size: 14px; box-sizing: border-box; }
    .in.sm { width: 120px; }
    .in:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-soft); }
    .in:disabled { opacity: 0.6; }
    .in::placeholder { color: var(--faint); }

    .ta { width: 100%; box-sizing: border-box; resize: vertical; padding: 12px 14px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); color: var(--fg); font: inherit; font-size: 14px; line-height: 1.55; }
    .ta:focus { outline: none; border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-soft); }
    .ta:disabled { opacity: 0.6; }
    .ta::placeholder { color: var(--faint); }

    .seg { display: inline-flex; gap: 4px; padding: 4px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); align-self: flex-start; flex-wrap: wrap; }
    .seg-btn { height: 30px; padding: 0 13px; border: 0; border-radius: calc(var(--radius) - 2px); background: transparent; font: inherit; font-size: 13px; color: var(--muted); cursor: pointer; }
    .seg-btn:hover:not(:disabled):not(.on) { color: var(--fg); }
    .seg-btn.on { background: var(--accent-soft); color: var(--accent); font-weight: 500; }
    .seg-btn:disabled { opacity: 0.6; cursor: default; }
    .seg-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .in.reuse { width: 100%; }

    .actions { display: flex; align-items: center; gap: 16px; flex-wrap: wrap; }
    .note { color: var(--faint); font-size: 12.5px; line-height: 1.5; margin: 0; max-width: 48ch; }

    .btn { display: inline-flex; align-items: center; gap: 8px; height: 38px; padding: 0 16px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 14px; font-weight: 500; color: inherit; cursor: pointer; }
    .btn:hover:not(:disabled) { border-color: var(--fg); }
    .btn:disabled { opacity: 0.6; cursor: default; }
    .btn-primary { background: var(--accent); color: oklch(97% 0.02 40); border-color: var(--accent); }
    .btn-primary:hover:not(:disabled) { background: var(--accent-2); border-color: var(--accent-2); }
    .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }

    .toast { position: fixed; bottom: 24px; left: 50%; transform: translateX(-50%); z-index: 80; background: var(--fg); color: oklch(97% 0.01 80); padding: 12px 18px; border-radius: 11px; font-size: 13.5px; box-shadow: 0 16px 40px -16px color-mix(in oklch, var(--fg) 60%, transparent); }
  `,
})
export class NewBuild {
  private readonly api = inject(ApiService);
  private readonly wasm = inject(WasmService);
  private readonly copilot = inject(CopilotHost);
  private readonly buildRunner = inject(BuildRunner);
  private readonly buildsStore = inject(BuildsStore);

  /** Which JD source is active — paste, URL fetch, or reuse a previous build. */
  protected readonly source = signal<JdSource>('paste');
  protected readonly jdText = signal('');
  protected readonly url = signal('');
  /** The previous build whose parsed JD to reuse (retailor). */
  protected readonly selectedBuildId = signal('');

  /** Past builds to pick from in the reuse source. */
  protected readonly builds = this.buildsStore.builds;

  protected readonly running = signal(false);
  protected readonly fetching = signal(false);
  private readonly _toast = signal<string | null>(null);
  protected readonly toast = this._toast.asReadonly();

  /** The Run button is enabled only when the active source has an input. */
  protected readonly canRun = computed(() =>
    this.source() === 'reuse' ? !!this.selectedBuildId() : !!this.jdText().trim(),
  );

  constructor() {
    // The sidebar loads the list at app start, but land here directly (or on a
    // fresh reload) and it may be empty — load it so the reuse picker has data.
    if (this.builds().length === 0) this.buildsStore.load();
  }

  /** Fetch a cross-origin posting server-side and drop it into the textarea. */
  protected fetchJd(): void {
    const url = this.url().trim();
    if (!url) return;
    this.fetching.set(true);
    this.api.fetchJd(url).subscribe({
      next: (text) => {
        this.jdText.set(text);
        this.fetching.set(false);
      },
      error: (err: unknown) => {
        this.fetching.set(false);
        this.showToast(`Couldn’t fetch that URL: ${errMessage(err)}. Paste the text instead.`);
      },
    });
  }

  /** Resolve the JD from the active source, then hand off to {@link BuildRunner}
   *  (the shared gap → loop → project → persist → navigate path). Every failure
   *  — "no dataset", a reused build with no parsed JD, a concurrent-run refusal
   *  from `runWithUi`, or a missing LLM credential surfacing as an `/api/llm`
   *  error — surfaces a toast and re-enables the form, never a dead spinner. */
  protected async runBuild(): Promise<void> {
    // Don't start a build while any copilot (here or in the tailoring workspace)
    // holds the shared modal — a concurrent run would hang it (CopilotHost.ask).
    if (this.running() || !this.canRun() || this.copilot.running()) return;

    this.running.set(true);
    try {
      let dataset;
      try {
        dataset = await firstValueFrom(this.api.getDataset());
      } catch (err) {
        if (err instanceof HttpErrorResponse && err.status === 404) {
          this.showToast('No dataset yet: add your experience first.');
          return;
        }
        throw err;
      }

      if (this.source() === 'reuse') {
        // Retailor: reuse the chosen build's already-parsed JD (no re-parse) and
        // run it against the current dataset via the shared path.
        const detail = await firstValueFrom(this.api.getBuild(this.selectedBuildId()));
        const jd = detail.jd;
        if (!jd) {
          this.showToast('That build has no parsed job description to reuse.');
          return;
        }
        // A cancelled run is confirmed by the app-global notice BuildRunner
        // fires after navigation (survives this component unmounting) — no
        // local toast, which would be lost on the route change anyway.
        await this.buildRunner.runAndSave(jd, dataset, 'Retailor');
      } else {
        const jd: JobRequirements = await this.wasm.parseJd(this.jdText().trim());
        await this.buildRunner.runAndSave(jd, dataset);
      }
    } catch (err) {
      this.showToast(errMessage(err));
    } finally {
      this.running.set(false);
    }
  }

  protected asValue(ev: Event): string {
    return (ev.target as HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement).value;
  }

  private showToast(msg: string): void {
    this._toast.set(msg);
    setTimeout(() => this._toast.set(null), 4000);
  }
}

/** A user-facing message from any thrown error (HTTP, Error, string, unknown).
 *  The server envelope is `{ error: { kind, message } }`; some paths send a bare
 *  string or `{ message }`; the wasm core rejects with plain strings (e.g. a
 *  missing LLM credential surfaced from `/api/llm`). Surface the real message. */
function errMessage(err: unknown): string {
  if (err instanceof HttpErrorResponse) {
    const b = err.error as { error?: { message?: string }; message?: string } | string | null;
    if (typeof b === 'string') return b || err.message;
    return b?.error?.message ?? b?.message ?? err.message;
  }
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  return 'Something went wrong: the run didn’t complete.';
}
