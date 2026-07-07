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
  GenerateCoverResponse,
  SaveCoverPayloadResponse,
  CoverBrief,
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

  /** `POST /api/builds/:id/edits` — save the workspace's local edits INTO the
   *  stored build: the server applies them to the canonical draft under the
   *  never-fabricate guards, re-renders both PDFs, and appends each to the
   *  build's on-disk edit log (for cross-session undo). `target` is `'summary'`
   *  or `'bullet:<source_id>'` — the caller translates its positional preview
   *  keys to canonical ids before sending. Returns how many landed and the new
   *  log length. */
  saveBuildEdits(
    id: string,
    edits: Array<{ target: string; text: string }>,
  ): Observable<{ saved: number; log_len: number }> {
    return this.http.post<{ saved: number; log_len: number }>(
      `${this.base}/builds/${encodeURIComponent(id)}/edits`,
      { edits },
    );
  }

  /** `POST /api/builds/:id/triage` — replace this build's objection triage (the
   *  ids "left for now"). A full replacement, so a leave is a save with the id
   *  added and a reopen a save with it removed. The payload is tiny, so the
   *  caller saves on every change and reverts its optimistic signal on failure. */
  saveTriage(id: string, left: string[]): Observable<{ status: string }> {
    return this.http.post<{ status: string }>(
      `${this.base}/builds/${encodeURIComponent(id)}/triage`,
      { left },
    );
  }

  /** `DELETE /api/builds/:id` — remove a build and every artifact under it, the
   *  same on-disk deletion `aarg history rm` performs. Returns the removed id;
   *  a missing build is a 404. */
  removeBuild(id: string): Observable<{ removed: string }> {
    return this.http.delete<{ removed: string }>(
      `${this.base}/builds/${encodeURIComponent(id)}`,
    );
  }

  /** `POST /api/builds/:id/cover` — draft (or redraft) a cover letter for the
   *  build server-side: the same `CoverLetterAgent` the CLI's `aarg cover` runs.
   *  It grounds the letter in the build's canonical résumé and JD, renders
   *  `cover_letter.pdf` into the build, and returns the letter plus any
   *  never-fabricate warnings. `brief` is optional — the result of a prior
   *  `cover_interview_interactive` session (the "Draft with copilot" flow);
   *  omitted, it drafts plainly, exactly as before that copilot existed. The
   *  JSON content-type header is mandatory either way (the route sits behind
   *  the same content-type gate the paid `/api/llm` route does). */
  generateCover(id: string, brief?: CoverBrief): Observable<GenerateCoverResponse> {
    return this.http.post<GenerateCoverResponse>(
      `${this.base}/builds/${encodeURIComponent(id)}/cover`,
      brief ? { brief } : {},
      { headers: { 'Content-Type': 'application/json' } },
    );
  }

  /** `POST /api/builds/:id/cover-brief` — the Cover Letter Editing view's
   *  "confirm as evidence" action: append one paragraph's own text to this
   *  build's `CoverBrief.emphasis` and persist `cover_brief.json`. Returns the
   *  brief as saved, so the caller can re-run `checkCoverProvenance` locally
   *  against it immediately, without a second round trip to re-fetch the
   *  build. Idempotent by exact text — confirming the same paragraph twice
   *  appends it once. */
  confirmCoverEvidence(id: string, text: string): Observable<{ brief: CoverBrief }> {
    return this.http.post<{ brief: CoverBrief }>(
      `${this.base}/builds/${encodeURIComponent(id)}/cover-brief`,
      { text },
    );
  }

  /** `PUT /api/builds/:id/cover-payload` — persist the Cover Letter Editing
   *  view's hand-edited paragraphs into the stored build and re-render the PDF.
   *  The server re-runs the same digit guard generation does: a paragraph
   *  introducing a figure the résumé and brief don't state is dropped (returned
   *  in `dropped`), never silently saved. Returns the letter as saved (surviving
   *  paragraphs only) so the caller can sync its local state, and the PDF
   *  filename to refetch for the pixel preview. The greeting and sign-off are
   *  code-filled and never sent — they carry over from the stored payload. The
   *  JSON content-type header is mandatory (the route sits behind the same gate
   *  the other body-taking build routes do). */
  saveCoverPayload(id: string, paragraphs: string[]): Observable<SaveCoverPayloadResponse> {
    return this.http.put<SaveCoverPayloadResponse>(
      `${this.base}/builds/${encodeURIComponent(id)}/cover-payload`,
      { paragraphs },
      { headers: { 'Content-Type': 'application/json' } },
    );
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

  /** `PUT /api/dataset` — persist an edited dataset. The response is a status
   *  acknowledgement (`{status:"saved"}`), NOT the dataset — callers must keep
   *  the object they sent (storing the ack as the dataset wedges every later
   *  provenance check and copilot run). */
  putDataset(dataset: ResumeDataset): Observable<{ status: string }> {
    return this.http.put<{ status: string }>(`${this.base}/dataset`, dataset);
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

  /** `GET /api/templates` — the template names available per variant (the
   *  built-ins plus any user human templates in the workspace), the exact set
   *  `POST /api/render` will accept. Feeds the workspace's template picker. */
  getTemplates(): Observable<{ ats: string[]; human: string[] }> {
    return this.http.get<{ ats: string[]; human: string[] }>(`${this.base}/templates`);
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
