import { Injectable, inject } from '@angular/core';
import { Router } from '@angular/router';
import { firstValueFrom } from 'rxjs';

import { ApiService } from './api.service';
import { WasmService } from './wasm.service';
import { CopilotHost } from '../shared/copilot-host';
import type {
  AdversarialReport,
  JobRequirements,
  ResumeDataset,
  TailoredResume,
  TokenUsage,
  VariantPayload,
} from '../models';

/** The one shared "regenerate a résumé for a JD against the dataset" operation.
 *
 *  Both entry points — New Build (a freshly parsed JD) and Retailor (an existing
 *  build's JD re-run against the user's current, possibly copilot-enriched
 *  dataset) — funnel through {@link runAndSave}, so the gap → adversarial loop →
 *  human projection → persist → navigate sequence lives in exactly one place.
 *
 *  The run happens inside {@link CopilotHost.runWithUi}, which lights up the
 *  shared progress overlay (iteration / score / live cost) for free and refuses
 *  a concurrent run (it throws; callers catch and toast). */
@Injectable({ providedIn: 'root' })
export class BuildRunner {
  private readonly wasm = inject(WasmService);
  private readonly api = inject(ApiService);
  private readonly copilot = inject(CopilotHost);
  private readonly router = inject(Router);

  /** Run the adversarial tailor loop for `jd` against `dataset`, persist the
   *  build via `POST /api/builds`, navigate to its tailoring workspace, and
   *  return the new build id plus whether a Stop cut the loop short. `label`
   *  names the run in the progress overlay ("New build" vs "Retailor").
   *
   *  A cancelled loop still returns the best draft it had, so it's saved and
   *  navigated to exactly like a full run; `cancelled` lets the caller note it. */
  async runAndSave(
    jd: JobRequirements,
    dataset: ResumeDataset,
    label = 'New build',
  ): Promise<{ id: string; cancelled: boolean }> {
    const result = await this.copilot.runWithUi(
      label,
      async () => {
        const gap = await this.wasm.analyzeGapLlm(jd, dataset);
        const loop = await this.wasm.tailorLoop(dataset, jd, gap, {
          revisions: 2,
          acceptable_score: 0.85,
        });
        // The loop has returned its best draft — nothing left in this run
        // (projectHuman, save) can honor a stop, so retire the Stop affordance
        // now rather than leaving it clickable through the tail of the run.
        this.copilot.endCancellable();
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
        return { id: created.id, cancelled: Boolean(loop['cancelled']) };
      },
      { cancellable: true },
    );

    await this.router.navigate(['/build', result.id, 'tailor']);
    // The app-global notice survives this navigation (it renders in
    // CopilotOverlay at the root), so it's the single, navigation-proof
    // confirmation that a stop was honored — the callers no longer toast it.
    if (result.cancelled) {
      this.copilot.notify('Stopped: saved the best draft so far.');
    }
    return result;
  }
}
