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
   *  return the new build id. `label` names the run in the progress overlay
   *  ("New build" vs "Retailor"). */
  async runAndSave(
    jd: JobRequirements,
    dataset: ResumeDataset,
    label = 'New build',
  ): Promise<string> {
    const id = await this.copilot.runWithUi(label, async () => {
      const gap = await this.wasm.analyzeGapLlm(jd, dataset);
      const loop = await this.wasm.tailorLoop(dataset, jd, gap, {
        revisions: 2,
        acceptable_score: 0.85,
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
    return id;
  }
}
