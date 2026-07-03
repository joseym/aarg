import { Injectable } from '@angular/core';

import type {
  ResumeDataset,
  JobRequirements,
  GapReport,
  VariantPayload,
  AdversarialReport,
  ProvenanceReport,
  WeightedCoverage,
  Objection,
  Models,
} from '../models';

/** A JS callback the core hands one JSON string and awaits a JSON string from.
 *  `llm` gets a `CompletionRequest`, returns a `CompletionResponse`; `user`
 *  gets a prompt envelope, returns an answer; `on_progress` is fire-and-forget. */
type StringCallback = (json: string) => string | Promise<string>;
type ProgressCallback = (json: string) => void;

/** Resolve after `ms` milliseconds — the backoff wait between LLM-proxy retries. */
function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** One parsed SSE `data:` payload from `/api/llm`'s streaming mode. */
type SseEvent =
  | { delta: string }
  | { done: { stop_reason: string | null; usage: unknown; model: string } }
  | { error: string; retryable?: boolean };

/** A streamed `/api/llm` response that died mid-flight — a read error, or the
 *  stream ending before its `done` frame. Transient like a dropped POST, so the
 *  retry wrapper restarts from zero. `chunks` is how many deltas had arrived, so
 *  the surfaced message can say "after N streamed chunks". */
class StreamTransportError extends Error {
  constructor(
    message: string,
    readonly chunks: number,
  ) {
    super(message);
    this.name = 'StreamTransportError';
  }
}

/** An `error` FRAME the server sent mid-stream — an application failure (a
 *  provider reject, an outage), NOT a transport blip, so the retry wrapper does
 *  not retry it. Carries the server's own chained message. */
class StreamAppError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'StreamAppError';
  }
}

/** Best-effort text for a thrown value. */
function errText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** The earliest SSE frame boundary in `buffer`: a blank line, written either as
 *  `\n\n` or `\r\n\r\n`. The Rust SSE parser only splits on `\n\n` (EX-003), so
 *  this reader handles both endings itself. Returns the boundary's start index
 *  and length, or null when no complete frame is buffered yet. */
function nextFrameSep(buffer: string): { index: number; length: number } | null {
  const crlf = buffer.indexOf('\r\n\r\n');
  const lf = buffer.indexOf('\n\n');
  if (crlf === -1 && lf === -1) return null;
  if (crlf === -1) return { index: lf, length: 2 };
  if (lf === -1) return { index: crlf, length: 4 };
  // Both present: take whichever ends the earlier frame.
  return crlf <= lf ? { index: crlf, length: 4 } : { index: lf, length: 2 };
}

/** Parse one SSE frame's `data:` payload(s) into an event, or null for a frame
 *  with no data line or an unparseable one (a comment/heartbeat, say). Multiple
 *  `data:` lines join with a newline, per the SSE spec. */
function parseSseFrame(frame: string): SseEvent | null {
  const data = frame
    .split(/\r\n|\n/)
    .filter((line) => line.startsWith('data:'))
    .map((line) => line.slice(5).replace(/^ /, ''))
    .join('\n');
  if (!data) return null;
  try {
    return JSON.parse(data) as SseEvent;
  } catch {
    return null;
  }
}

/** The subset of the generated `aarg_wasm.js` exports this service wraps. Typed
 *  locally (rather than importing the generated `.d.ts`) so the app still
 *  type-checks in a clone where `src/wasm/pkg` hasn't been built yet — the
 *  module is only ever loaded through the dynamic import below. */
interface WasmExports {
  default: (input?: unknown) => Promise<unknown>;

  // deterministic (pure) — JSON string in, JSON string out
  validate(datasetJson: string): string;
  analyze_gap(jdJson: string, datasetJson: string): string;
  project_ats(canonicalJson: string): string;
  check_claims(canonicalJson: string, payloadJson: string): string;
  check_provenance(canonicalJson: string, datasetJson: string): string;
  weighted_coverage(gapJson: string, jdJson: string): string;
  normalize_dashes(text: string): string;
  scrub_resume(canonicalJson: string): string;
  backed_phrases(jdJson: string, datasetJson: string): string;
  keyword_key(name: string): string;

  // callback-driven — the trailing js_sys::Function params are JS callbacks
  parse_jd_llm(jdText: string, modelsJson: string, llm: StringCallback): Promise<string>;
  analyze_gap_llm(
    jdJson: string,
    datasetJson: string,
    modelsJson: string,
    llm: StringCallback,
  ): Promise<string>;
  tailor_draft(
    datasetJson: string,
    jdJson: string,
    gapJson: string,
    modelsJson: string,
    llm: StringCallback,
  ): Promise<string>;
  review_draft(
    canonicalJson: string,
    jdJson: string,
    datasetJson: string,
    modelsJson: string,
    llm: StringCallback,
  ): Promise<string>;
  project_human_llm(
    canonicalJson: string,
    datasetJson: string,
    jdJson: string,
    modelsJson: string,
    llm: StringCallback,
  ): Promise<string>;
  refine_layout_llm(
    canonicalJson: string,
    datasetJson: string,
    jdJson: string,
    objectionJson: string,
    modelsJson: string,
    llm: StringCallback,
  ): Promise<string>;
  tailor_loop(
    datasetJson: string,
    jdJson: string,
    gapJson: string,
    paramsJson: string,
    modelsJson: string,
    llm: StringCallback,
    onProgress: ProgressCallback,
  ): Promise<string>;
  cancel_tailor_loop(): void;
  reset_tailor_loop_cancel(): void;
  voice_rewrite(
    canonicalJson: string,
    samplesJson: string,
    modelsJson: string,
    llm: StringCallback,
  ): Promise<string>;

  // interactive copilots — an extra `user` callback drives human-in-the-loop
  capture_metrics_interactive(
    datasetJson: string,
    reportJson: string,
    modelsJson: string,
    llm: StringCallback,
    user: StringCallback,
  ): Promise<string>;
  strengthen_interactive(
    datasetJson: string,
    reportJson: string,
    modelsJson: string,
    llm: StringCallback,
    user: StringCallback,
  ): Promise<string>;
  refine_summary_interactive(
    datasetJson: string,
    concern: string,
    modelsJson: string,
    llm: StringCallback,
    user: StringCallback,
  ): Promise<string>;
  enrich_roles_interactive(
    datasetJson: string,
    modelsJson: string,
    llm: StringCallback,
    user: StringCallback,
  ): Promise<string>;
  tune_interactive(
    canonicalJson: string,
    datasetJson: string,
    modelsJson: string,
    llm: StringCallback,
    user: StringCallback,
  ): Promise<string>;
  verify_skills_interactive(
    datasetJson: string,
    jdJson: string,
    gapJson: string,
    modelsJson: string,
    llm: StringCallback,
    user: StringCallback,
  ): Promise<string>;
  verify_skill_interactive(
    datasetJson: string,
    jdJson: string,
    keyword: string,
    modelsJson: string,
    llm: StringCallback,
    user: StringCallback,
  ): Promise<string>;
}

// ── payload shapes the compound exports return ──────────────────────────
export interface TailorOutcome {
  resume: unknown;
  warnings: string[];
  dropped_unrecorded: unknown[];
}
export interface TailorLoopResult {
  resume: unknown;
  /** True when a stop was *requested* during the run — not necessarily that
   *  work was skipped (the request may land on the final pass). The best draft
   *  is in `resume` either way. */
  cancelled?: boolean;
  [key: string]: unknown;
}

/** Loads the deterministic + interactive domain core (compiled to wasm) and
 *  exposes its exports as typed methods. JSON crosses the boundary as strings;
 *  this service parses/stringifies so callers work in objects.
 *
 *  `init()` (the module's default export) is called once and memoised. The
 *  callback-driven exports need three JS functions:
 *   - `llm`   → wired here to `POST /api/llm` (a `CompletionRequest` in, a
 *              `CompletionResponse` out) so the core never sees a key.
 *   - `user` / `on_progress` → injectable handlers a later wave replaces with
 *              modal-backed implementations; the defaults are safe no-ops. */
@Injectable({ providedIn: 'root' })
export class WasmService {
  private mod?: WasmExports;
  private loading?: Promise<WasmExports>;

  /** Per-tier model ids passed to the callback exports. A later wave lets the
   *  user configure this; the default names the flagship for every tier. */
  models: Models = { model: 'claude-opus-4-8' };

  /** Supplies human-in-the-loop answers for the interactive copilots. Receives
   *  the prompt envelope JSON, returns the answer JSON. Default: refuses, so a
   *  copilot invoked before the modals exist fails loudly instead of hanging
   *  (the browser mirror of `NonInteractiveUser`). */
  userHandler: StringCallback = () => {
    throw new Error('no interactive user handler is wired up yet');
  };

  /** Receives live progress events from `tailor_loop` (cost, iteration). Default
   *  is a no-op; the tailoring wave points this at the loop UI. */
  progressHandler: ProgressCallback = () => {};

  /** Ticks the cumulative character count of the in-flight streamed `/api/llm`
   *  completion, so the overlay can show tokens arriving. Reset to 0 at the
   *  start of each call (and each retry). Null by default; `CopilotHost` wires
   *  it to a signal. Display only — cost/usage stay milestone-driven. */
  streamHandler: ((chars: number) => void) | null = null;

  /** Lazy-load + init the wasm module exactly once. */
  private async load(): Promise<WasmExports> {
    if (this.mod) return this.mod;
    this.loading ??= (async () => {
      const mod = (await import('../../wasm/pkg/aarg_wasm.js')) as unknown as WasmExports;
      await mod.default();
      this.mod = mod;
      await this.loadModels();
      return mod;
    })();
    return this.loading;
  }

  /** Adopt the server's configured model tiers (`GET /api/models`), so an
   *  in-browser build spends the same cheap/mid/smart mix the CLI does instead
   *  of running every call on one hardcoded model. Best-effort: if the endpoint
   *  is unreachable the built-in default stands, and any real credential/LLM
   *  problem still surfaces on the first `/api/llm` call. */
  private async loadModels(): Promise<void> {
    try {
      const res = await fetch('/api/models');
      if (!res.ok) return;
      const t = (await res.json()) as { cheap?: string; mid?: string; smart?: string };
      if (t.cheap && t.mid && t.smart) {
        // `model` mirrors the smart tier — the "headline" model the cost
        // estimate and the saved build's meta record.
        this.models = { cheap: t.cheap, mid: t.mid, smart: t.smart, model: t.smart };
      }
    } catch {
      // Keep the default; the LLM path will report a genuine failure itself.
    }
  }

  /** JSON callback bound to the LLM proxy. Posts the `CompletionRequest` string
   *  the core produced straight to `/api/llm` and returns the raw
   *  `CompletionResponse` JSON string the core expects.
   *
   *  A single completion can run 60-90s, and on a phone iOS Safari kills a
   *  long in-flight POST when the screen locks or the tab is backgrounded — it
   *  surfaces as a fetch `TypeError` ("Load failed"). Providers also shed load
   *  with a 429/529. Both are transient, so retry a bounded number of times
   *  with backoff before giving up. Real application errors (a bad request, a
   *  missing credential, a 500 with a body) are not transient and reject at
   *  once — retrying them would only stall the run. Each attempt is a fresh
   *  `fetch` with no client-side timeout, so backoff waits never stack onto a
   *  call's own duration. */
  private readonly llm: StringCallback = async (requestJson) => {
    // Backoff before the 1st and 2nd retry; its length is the retry budget (2).
    const backoffMs = [2000, 5000];
    for (let attempt = 0; ; attempt++) {
      // Each attempt starts the stream tick from zero — a retry re-accumulates.
      this.streamHandler?.(0);
      let res: Response;
      try {
        res = await fetch('/api/llm', {
          method: 'POST',
          // Ask for SSE; the server streams when it can (a tool-free request)
          // and falls back to a buffered JSON body otherwise. We branch on the
          // RESPONSE content type below, never on this request header.
          headers: { 'content-type': 'application/json', accept: 'text/event-stream' },
          body: requestJson,
        });
      } catch (err) {
        // Transport-level failure (network dropped, request aborted by the OS).
        if (attempt < backoffMs.length) {
          console.warn(`/api/llm transport error; retry ${attempt + 1} after backoff`, err);
          await delay(backoffMs[attempt]);
          continue;
        }
        throw err instanceof Error ? err : new Error(String(err));
      }
      if (!res.ok) {
        // A provider overload (429 rate limit / 529 overloaded) is transient too.
        if ((res.status === 429 || res.status === 529) && attempt < backoffMs.length) {
          console.warn(`/api/llm overloaded (${res.status}); retry ${attempt + 1} after backoff`);
          await delay(backoffMs[attempt]);
          continue;
        }
        // Any other non-2xx (or an exhausted overload budget) is surfaced with
        // the server's body so an actionable message (a missing credential, a
        // Typst failure, an overload note) survives instead of a bare code.
        const body = await res.text().catch(() => '');
        throw new Error(body || `/api/llm returned ${res.status}`);
      }

      // Buffered fallback: no `text/event-stream` content type means the server
      // returned the whole `CompletionResponse` as JSON — exactly today's path.
      const ctype = res.headers.get('content-type') ?? '';
      if (!ctype.includes('text/event-stream')) return res.text();

      // Streamed: read the SSE frames, accumulate the text, and resolve with a
      // full `CompletionResponse` JSON string (the unchanged wasm bridge
      // contract). A stream that dies mid-flight is a retryable transport
      // failure; an `error` frame is an application error and is not retried.
      try {
        return await this.readSseStream(res);
      } catch (err) {
        if (err instanceof StreamAppError) throw new Error(err.message);
        if (err instanceof StreamTransportError && attempt < backoffMs.length) {
          console.warn(`/api/llm ${err.message}; retry ${attempt + 1} after backoff`);
          await delay(backoffMs[attempt]);
          continue;
        }
        throw err instanceof Error ? err : new Error(String(err));
      }
    }
  };

  /** Read an SSE `/api/llm` response to completion: accumulate `delta` frames
   *  (ticking `streamHandler` with the running char count), resolve with a
   *  `CompletionResponse` JSON string on the `done` frame, throw a
   *  {@link StreamAppError} on an `error` frame, and throw a
   *  {@link StreamTransportError} if the stream dies or ends without `done`. */
  private async readSseStream(res: Response): Promise<string> {
    if (!res.body) throw new StreamTransportError('the streamed response had no body', 0);
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    let accumulated = '';
    let chunks = 0;
    try {
      for (;;) {
        let read: ReadableStreamReadResult<Uint8Array>;
        try {
          read = await reader.read();
        } catch (err) {
          throw new StreamTransportError(
            `the stream failed after ${chunks} streamed chunks: ${errText(err)}`,
            chunks,
          );
        }
        if (read.done) {
          // The stream closed before a `done` frame — an incomplete completion.
          throw new StreamTransportError(
            `the stream ended without a done frame after ${chunks} streamed chunks`,
            chunks,
          );
        }
        buffer += decoder.decode(read.value, { stream: true });
        for (let sep = nextFrameSep(buffer); sep; sep = nextFrameSep(buffer)) {
          const frame = buffer.slice(0, sep.index);
          buffer = buffer.slice(sep.index + sep.length);
          const evt = parseSseFrame(frame);
          if (!evt) continue;
          if ('delta' in evt) {
            accumulated += evt.delta;
            chunks++;
            this.streamHandler?.(accumulated.length);
          } else if ('done' in evt) {
            return JSON.stringify({
              text: accumulated,
              tool_calls: [],
              model: evt.done.model,
              stop_reason: evt.done.stop_reason ?? null,
              usage: evt.done.usage,
            });
          } else if ('error' in evt) {
            // The server tags transient upstream failures (in-stream
            // overloads, the streaming twin of a 529) so they re-enter the
            // retry budget instead of aborting the run.
            if (evt.retryable) throw new TypeError(evt.error);
            throw new StreamAppError(evt.error);
          }
        }
      }
    } finally {
      // Best-effort: release the connection whether we resolved, errored, or the
      // caller stopped reading. `cancel` rejects only on an already-errored
      // stream, which is fine to ignore.
      void reader.cancel().catch(() => undefined);
    }
  }

  private modelsJson(): string {
    return JSON.stringify(this.models);
  }

  // ── deterministic (pure) exports ──────────────────────────────────────

  async validate(dataset: ResumeDataset): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(m.validate(JSON.stringify(dataset)));
  }

  async analyzeGap(jd: JobRequirements, dataset: ResumeDataset): Promise<GapReport> {
    const m = await this.load();
    return JSON.parse(m.analyze_gap(JSON.stringify(jd), JSON.stringify(dataset)));
  }

  async projectAts(canonical: unknown): Promise<VariantPayload> {
    const m = await this.load();
    return JSON.parse(m.project_ats(JSON.stringify(canonical)));
  }

  async checkClaims(canonical: unknown, payload: VariantPayload): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(m.check_claims(JSON.stringify(canonical), JSON.stringify(payload)));
  }

  async checkProvenance(canonical: unknown, dataset: ResumeDataset): Promise<ProvenanceReport> {
    const m = await this.load();
    return JSON.parse(m.check_provenance(JSON.stringify(canonical), JSON.stringify(dataset)));
  }

  async weightedCoverage(gap: GapReport, jd: JobRequirements): Promise<WeightedCoverage> {
    const m = await this.load();
    return JSON.parse(m.weighted_coverage(JSON.stringify(gap), JSON.stringify(jd)));
  }

  /** Punctuation normaliser — plain text in, plain text out (no JSON). */
  async normalizeDashes(text: string): Promise<string> {
    const m = await this.load();
    return m.normalize_dashes(text);
  }

  async scrubResume(canonical: unknown): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(m.scrub_resume(JSON.stringify(canonical)));
  }

  async backedPhrases(jd: JobRequirements, dataset: ResumeDataset): Promise<string[]> {
    const m = await this.load();
    return JSON.parse(m.backed_phrases(JSON.stringify(jd), JSON.stringify(dataset)));
  }

  async keywordKey(name: string): Promise<string> {
    const m = await this.load();
    return JSON.parse(m.keyword_key(name));
  }

  // ── callback-driven exports (llm) ─────────────────────────────────────

  async parseJd(jdText: string): Promise<JobRequirements> {
    const m = await this.load();
    return JSON.parse(await m.parse_jd_llm(jdText, this.modelsJson(), this.llm));
  }

  async analyzeGapLlm(jd: JobRequirements, dataset: ResumeDataset): Promise<GapReport> {
    const m = await this.load();
    return JSON.parse(
      await m.analyze_gap_llm(
        JSON.stringify(jd),
        JSON.stringify(dataset),
        this.modelsJson(),
        this.llm,
      ),
    );
  }

  async tailorDraft(
    dataset: ResumeDataset,
    jd: JobRequirements,
    gap: GapReport,
  ): Promise<TailorOutcome> {
    const m = await this.load();
    return JSON.parse(
      await m.tailor_draft(
        JSON.stringify(dataset),
        JSON.stringify(jd),
        JSON.stringify(gap),
        this.modelsJson(),
        this.llm,
      ),
    );
  }

  async reviewDraft(
    canonical: unknown,
    jd: JobRequirements,
    dataset: ResumeDataset,
  ): Promise<AdversarialReport> {
    const m = await this.load();
    return JSON.parse(
      await m.review_draft(
        JSON.stringify(canonical),
        JSON.stringify(jd),
        JSON.stringify(dataset),
        this.modelsJson(),
        this.llm,
      ),
    );
  }

  async projectHuman(
    canonical: unknown,
    dataset: ResumeDataset,
    jd: JobRequirements,
  ): Promise<VariantPayload> {
    const m = await this.load();
    return JSON.parse(
      await m.project_human_llm(
        JSON.stringify(canonical),
        JSON.stringify(dataset),
        JSON.stringify(jd),
        this.modelsJson(),
        this.llm,
      ),
    );
  }

  async refineLayout(
    canonical: unknown,
    dataset: ResumeDataset,
    jd: JobRequirements,
    objection: Objection,
  ): Promise<VariantPayload> {
    const m = await this.load();
    return JSON.parse(
      await m.refine_layout_llm(
        JSON.stringify(canonical),
        JSON.stringify(dataset),
        JSON.stringify(jd),
        JSON.stringify(objection),
        this.modelsJson(),
        this.llm,
      ),
    );
  }

  async voiceRewrite(canonical: unknown, samples: string[]): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.voice_rewrite(
        JSON.stringify(canonical),
        JSON.stringify(samples),
        this.modelsJson(),
        this.llm,
      ),
    );
  }

  /** The adversarial loop (FR-3.x): tailor → review → revise, capped, returning
   *  the best draft. `params` = `{ revisions?, acceptable_score? }`. Progress
   *  events flow to `progressHandler`. */
  async tailorLoop(
    dataset: ResumeDataset,
    jd: JobRequirements,
    gap: GapReport,
    params: { revisions?: number; acceptable_score?: number } = {},
  ): Promise<TailorLoopResult> {
    const m = await this.load();
    return JSON.parse(
      await m.tailor_loop(
        JSON.stringify(dataset),
        JSON.stringify(jd),
        JSON.stringify(gap),
        JSON.stringify(params),
        this.modelsJson(),
        this.llm,
        (json) => this.progressHandler(json),
      ),
    );
  }

  /** Ask an in-flight tailor loop to stop after its current pass. */
  async cancelTailorLoop(): Promise<void> {
    (await this.load()).cancel_tailor_loop();
  }

  /** Arm a fresh cancellable run: clear any stale stop request. The host calls
   *  this when a run *begins* (before gap analysis), so a Stop pressed at any
   *  point of the run survives to the loop's first check. */
  async resetTailorLoopCancel(): Promise<void> {
    (await this.load()).reset_tailor_loop_cancel();
  }

  // ── interactive copilots (llm + user) ─────────────────────────────────

  async captureMetrics(dataset: ResumeDataset, report: AdversarialReport): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.capture_metrics_interactive(
        JSON.stringify(dataset),
        JSON.stringify(report),
        this.modelsJson(),
        this.llm,
        (json) => this.userHandler(json),
      ),
    );
  }

  async strengthen(dataset: ResumeDataset, report: AdversarialReport): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.strengthen_interactive(
        JSON.stringify(dataset),
        JSON.stringify(report),
        this.modelsJson(),
        this.llm,
        (json) => this.userHandler(json),
      ),
    );
  }

  async refineSummary(dataset: ResumeDataset, concern: string): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.refine_summary_interactive(
        JSON.stringify(dataset),
        concern,
        this.modelsJson(),
        this.llm,
        (json) => this.userHandler(json),
      ),
    );
  }

  async enrichRoles(dataset: ResumeDataset): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.enrich_roles_interactive(
        JSON.stringify(dataset),
        this.modelsJson(),
        this.llm,
        (json) => this.userHandler(json),
      ),
    );
  }

  async tune(canonical: unknown, dataset: ResumeDataset): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.tune_interactive(
        JSON.stringify(canonical),
        JSON.stringify(dataset),
        this.modelsJson(),
        this.llm,
        (json) => this.userHandler(json),
      ),
    );
  }

  async verifySkills(
    dataset: ResumeDataset,
    jd: JobRequirements,
    gap: GapReport,
  ): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.verify_skills_interactive(
        JSON.stringify(dataset),
        JSON.stringify(jd),
        JSON.stringify(gap),
        this.modelsJson(),
        this.llm,
        (json) => this.userHandler(json),
      ),
    );
  }

  /** Verify a single JD requirement the user clicked (coverage-map row action),
   *  scoped to just that keyword — see `verify_skill_interactive`. */
  async verifySkill(dataset: ResumeDataset, jd: JobRequirements, keyword: string): Promise<unknown> {
    const m = await this.load();
    return JSON.parse(
      await m.verify_skill_interactive(
        JSON.stringify(dataset),
        JSON.stringify(jd),
        keyword,
        this.modelsJson(),
        this.llm,
        (json) => this.userHandler(json),
      ),
    );
  }
}
