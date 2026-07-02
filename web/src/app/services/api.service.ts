import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable, map } from 'rxjs';

import type {
  BuildSummary,
  BuildDetail,
  ResumeDataset,
  VariantPayload,
  CompletionRequest,
  CompletionResponse,
  CostEstimate,
  JobRequirements,
  GapReport,
  TailoredResume,
  AdversarialReport,
  TokenUsage,
} from '../models';

/** The `POST /api/builds` body: everything the browser's wasm tailor loop
 *  produced that the server needs to persist a numbered build. The ATS variant
 *  is re-projected server-side from `canonical`, so only the LLM-reworded
 *  `human_payload` is sent (and it's optional — a run may render ATS only). */
export interface CreateBuildRequest {
  jd: JobRequirements;
  gap_report: GapReport;
  canonical: TailoredResume;
  adversarial_report: AdversarialReport;
  human_payload?: VariantPayload;
  model: string;
  usage: TokenUsage;
}

/** Typed client for `aarg serve`'s HTTP API.
 *
 *  Same-origin in production (the binary serves `web/dist/aarg/browser`); in
 *  dev, `/api/*` is proxied to `http://127.0.0.1:8787` via `proxy.conf.json`.
 *  So every path is root-relative — no base URL to configure. */
@Injectable({ providedIn: 'root' })
export class ApiService {
  private readonly http = inject(HttpClient);
  private readonly base = '/api';

  // ── builds ──────────────────────────────────────────────────────────
  /** `GET /api/builds` — past builds, newest first. */
  getBuilds(): Observable<BuildSummary[]> {
    return this.http
      .get<{ builds: BuildSummary[] }>(`${this.base}/builds`)
      .pipe(map((r) => r.builds));
  }

  /** `GET /api/builds/:id` — one build's full artifact bundle. */
  getBuild(id: string): Observable<BuildDetail> {
    return this.http.get<BuildDetail>(`${this.base}/builds/${encodeURIComponent(id)}`);
  }

  /** `POST /api/builds` — persist a browser-run build (the wasm tailor loop's
   *  output) the way `aarg tailor` does, returning the new numbered build id. */
  createBuild(body: CreateBuildRequest): Observable<{ id: string }> {
    return this.http.post<{ id: string }>(`${this.base}/builds`, body);
  }

  /** `GET /api/builds/:id/files/:name` — a stored rendered PDF, as a blob. */
  getBuildFile(id: string, name: string): Observable<Blob> {
    return this.http.get(
      `${this.base}/builds/${encodeURIComponent(id)}/files/${encodeURIComponent(name)}`,
      { responseType: 'blob' },
    );
  }

  // ── dataset ─────────────────────────────────────────────────────────
  /** `GET /api/dataset` — the source-of-truth resume dataset. */
  getDataset(): Observable<ResumeDataset> {
    return this.http.get<ResumeDataset>(`${this.base}/dataset`);
  }

  /** `PUT /api/dataset` — persist an edited dataset. */
  putDataset(dataset: ResumeDataset): Observable<ResumeDataset> {
    return this.http.put<ResumeDataset>(`${this.base}/dataset`, dataset);
  }

  // ── render ──────────────────────────────────────────────────────────
  /** `POST /api/render` — render a variant payload to a PDF (Typst), as a blob. */
  render(
    variant: string,
    payload: VariantPayload,
    template?: string,
  ): Observable<Blob> {
    return this.http.post(
      `${this.base}/render`,
      { variant, payload, template },
      { responseType: 'blob' },
    );
  }

  // ── job description ─────────────────────────────────────────────────
  /** `POST /api/fetch-jd` — fetch a cross-origin posting server-side, returning
   *  its extracted text. */
  fetchJd(url: string): Observable<string> {
    return this.http
      .post<{ text: string }>(`${this.base}/fetch-jd`, { url })
      .pipe(map((r) => r.text));
  }

  // ── cost ────────────────────────────────────────────────────────────
  /** `GET /api/cost` — a dollar estimate for a token count (null on a plan). */
  getCost(model: string, input: number, output: number): Observable<CostEstimate> {
    const params = { model, input: String(input), output: String(output) };
    return this.http.get<CostEstimate>(`${this.base}/cost`, { params });
  }

  // ── llm ─────────────────────────────────────────────────────────────
  /** `POST /api/llm` — proxy one completion to the configured provider. This is
   *  the endpoint `WasmService` wires the core's `llm` callback to. */
  complete(request: CompletionRequest): Observable<CompletionResponse> {
    return this.http.post<CompletionResponse>(`${this.base}/llm`, request);
  }
}
