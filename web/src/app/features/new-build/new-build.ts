import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { Router } from '@angular/router';
import { HttpErrorResponse } from '@angular/common/http';
import { firstValueFrom } from 'rxjs';

import { ApiService } from '../../services/api.service';
import { WasmService } from '../../services/wasm.service';
import { CopilotHost } from '../../shared/copilot-host';
import type {
  AdversarialReport,
  TailoredResume,
  TokenUsage,
  VariantPayload,
} from '../../models';

/** `/new` — the New-Build screen. Paste (or fetch) a job description, then run
 *  the full adversarial tailor loop entirely in the browser: parse the JD →
 *  analyze the gap → drive the capped tailor/review/revise loop → project the
 *  human variant, and persist the result via `POST /api/builds`. The live
 *  iteration/score/cost overlay is the shared {@link CopilotHost} progress panel
 *  — running the loop inside `runWithUi` lights it up for free.
 *
 *  This is the same loop `aarg tailor` runs, live. It can't be cancelled mid-run
 *  yet — the loop exposes no stop seam — so the UI says so plainly and simply
 *  keeps the form disabled until the run settles (success or error). */
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
          drafts → reviews → revises a canonical résumé — the same loop the CLI
          runs, live in your browser.
        </p>
      </header>

      <!-- ── JD source ── -->
      <section class="field">
        <label class="lbl" for="jd">Job description</label>
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
        <textarea
          id="jd"
          class="ta"
          rows="12"
          placeholder="…or paste the full job description here."
          [value]="jdText()"
          (input)="jdText.set(asValue($event))"
          [disabled]="running()"
        ></textarea>
      </section>

      <!-- ── advanced (collapsible) ── -->
      <details class="adv">
        <summary>Advanced</summary>
        <div class="adv-grid">
          <label class="adv-field">
            <span>Revisions</span>
            <input
              class="in sm"
              type="number"
              min="0"
              max="5"
              step="1"
              [value]="revisions()"
              (input)="revisions.set(asNumber($event, 2))"
              [disabled]="running()"
            />
          </label>
          <label class="adv-field">
            <span>Acceptable score</span>
            <input
              class="in sm"
              type="number"
              min="0"
              max="1"
              step="0.05"
              [value]="acceptableScore()"
              (input)="acceptableScore.set(asNumber($event, 0.85))"
              [disabled]="running()"
            />
          </label>
        </div>
      </details>

      <div class="actions">
        <button
          class="btn btn-primary"
          type="button"
          (click)="runBuild()"
          [disabled]="running() || !jdText().trim()"
        >
          {{ running() ? 'Running…' : 'Run build' }}
        </button>
        <p class="note">
          Runs live and can’t be cancelled mid-run yet — the loop has no stop
          seam. Give it a minute.
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

    .adv { border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); padding: 4px 14px; }
    .adv summary { cursor: pointer; font-size: 13.5px; color: var(--muted); padding: 8px 0; }
    .adv summary:hover { color: var(--fg); }
    .adv-grid { display: flex; gap: 24px; padding: 6px 0 14px; flex-wrap: wrap; }
    .adv-field { display: flex; flex-direction: column; gap: 6px; font-size: 13px; color: var(--muted); }

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
  private readonly router = inject(Router);

  protected readonly jdText = signal('');
  protected readonly url = signal('');
  protected readonly revisions = signal(2);
  protected readonly acceptableScore = signal(0.85);

  protected readonly running = signal(false);
  protected readonly fetching = signal(false);
  private readonly _toast = signal<string | null>(null);
  protected readonly toast = this._toast.asReadonly();

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
        this.showToast(`Couldn’t fetch that URL: ${errMessage(err)} — paste the text instead.`);
      },
    });
  }

  /** Run the full adversarial loop in-browser, then persist the build. Every
   *  failure (including "no dataset" and a missing LLM credential surfacing as an
   *  `/api/llm` error) surfaces a toast and re-enables the form — never a dead
   *  spinner. */
  protected async runBuild(): Promise<void> {
    const jdText = this.jdText().trim();
    // Don't start a build while any copilot (here or in the tailoring workspace)
    // holds the shared modal — a concurrent run would hang it (CopilotHost.ask).
    if (!jdText || this.running() || this.copilot.running()) return;

    this.running.set(true);
    try {
      let dataset;
      try {
        dataset = await firstValueFrom(this.api.getDataset());
      } catch (err) {
        if (err instanceof HttpErrorResponse && err.status === 404) {
          this.showToast('No dataset yet — add your experience first.');
          return;
        }
        throw err;
      }

      const id = await this.copilot.runWithUi('New build', async () => {
        const jd = await this.wasm.parseJd(jdText);
        const gap = await this.wasm.analyzeGapLlm(jd, dataset);
        const loop = await this.wasm.tailorLoop(dataset, jd, gap, {
          revisions: this.revisions(),
          acceptable_score: this.acceptableScore(),
        });
        // ATS is re-projected server-side by the endpoint (never trusting a
        // client-supplied one), so only the LLM-reworded human variant is sent.
        const human = await this.wasm.projectHuman(loop.resume, dataset, jd);

        const created = await firstValueFrom(
          this.api.createBuild({
            jd,
            gap_report: gap,
            canonical: loop.resume as TailoredResume,
            adversarial_report: loop['report'] as AdversarialReport,
            human_payload: human as VariantPayload,
            model: this.wasm.models.model,
            usage: loop['usage'] as TokenUsage,
          }),
        );
        return created.id;
      });

      await this.router.navigate(['/build', id, 'tailor']);
    } catch (err) {
      this.showToast(errMessage(err));
    } finally {
      this.running.set(false);
    }
  }

  protected asValue(ev: Event): string {
    return (ev.target as HTMLInputElement | HTMLTextAreaElement).value;
  }

  protected asNumber(ev: Event, fallback: number): number {
    const v = Number((ev.target as HTMLInputElement).value);
    return Number.isFinite(v) ? v : fallback;
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
  return 'Something went wrong — the run didn’t complete.';
}
