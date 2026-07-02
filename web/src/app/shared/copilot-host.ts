import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injectable,
  computed,
  effect,
  inject,
  signal,
  viewChild,
} from '@angular/core';

import { WasmService } from '../services/wasm.service';
import { ApiService } from '../services/api.service';
import { normalizeDashes } from './normalize-dashes';

// ── prompt envelopes the core's `user` callback hands us ────────────────────
// These mirror the wasm bridge contract exactly: the core sends one of these as
// JSON, and awaits a JSON *string* answer whose shape depends on `kind`.
interface SelectEnvelope {
  kind: 'select';
  prompt: string;
  options: string[];
}
interface MultiSelectEnvelope {
  kind: 'multi_select';
  prompt: string;
  options: string[];
}
interface TextEnvelope {
  kind: 'text';
  prompt: string;
}
interface ConfirmEnvelope {
  kind: 'confirm';
  prompt: string;
  default: boolean;
}
interface NotifyEnvelope {
  kind: 'notify';
  message: string;
}

/** Any prompt envelope the core can send. */
export type Envelope =
  | SelectEnvelope
  | MultiSelectEnvelope
  | TextEnvelope
  | ConfirmEnvelope
  | NotifyEnvelope;

/** An envelope that actually waits on the user (everything but `notify`). */
export type QuestionEnvelope = Exclude<Envelope, NotifyEnvelope>;

/** A pending question: the parsed envelope plus the promise resolver the modal
 *  settles with a stringified answer.
 *
 *  `choices` carries the select/multi_select options as {display, value} pairs:
 *  `display` is the dash-normalized text SHOWN to the user, `value` is the
 *  ORIGINAL option string. The overlay renders `display` but answers with
 *  `value`, because the wasm bridge matches the chosen option by exact string —
 *  normalizing the answer would break the match. Empty for text/confirm. */
export interface PendingQuestion {
  env: QuestionEnvelope;
  resolve: (answer: string) => void;
  choices: { display: string; value: string }[];
}

/** A live progress event from `tailor_loop`. */
export interface ProgressEvent {
  phase:
    | 'drafting'
    | 'revising'
    | 'revision_drafted'
    | 'evaluated'
    | 'no_improvement'
    | 'done';
  iteration?: number;
  score?: number;
  usage?: { input_tokens: number; output_tokens: number };
  message: string;
}

/**
 * The human-in-the-loop host: the browser mirror of the CLI's interactive user.
 *
 * Installs the two `WasmService` seams in its constructor so that every
 * interactive copilot (`strengthen_interactive`, `capture_metrics_interactive`,
 * …) drives this modal, and every `tailor_loop` run streams progress here.
 * Owns the reactive state; `CopilotOverlay` renders it.
 */
@Injectable({ providedIn: 'root' })
export class CopilotHost {
  private readonly wasm = inject(WasmService);
  private readonly api = inject(ApiService);

  /** The question the modal is currently asking, or null when idle. */
  readonly question = signal<PendingQuestion | null>(null);
  /** A transient one-way `notify` message, or null. */
  readonly notice = signal<string | null>(null);
  /** Every progress event seen in the current run, in order. */
  readonly progress = signal<ProgressEvent[]>([]);
  /** A label for the in-flight operation, or null when nothing is running. */
  readonly running = signal<string | null>(null);
  /** Whether the in-flight run can be stopped (the overlay shows a Stop button
   *  only when true — set per-run via `runWithUi`'s options). */
  readonly cancellable = signal(false);
  /** Whether a Stop has been requested and we're waiting out the current pass. */
  readonly stopping = signal(false);
  /** Running token totals across the current run. */
  readonly usage = signal<{ input: number; output: number }>({ input: 0, output: 0 });
  /** A display cost — a dollar figure, a subscription note, or empty. */
  readonly costLabel = signal<string>('');

  private noticeTimer?: ReturnType<typeof setTimeout>;

  constructor() {
    // Install the seams: the core now asks the user through this host, and
    // streams progress into it. Instantiating the host is what wires them up.
    this.wasm.userHandler = (json) => this.ask(json);
    this.wasm.progressHandler = (json) => this.onProgress(json);
    // Harness hook: lets a headless (Playwright) test drive the modal directly,
    // e.g. `__copilotHost.ask('{"kind":"text","prompt":"hi"}')`. Dev convenience.
    (globalThis as unknown as { __copilotHost?: CopilotHost }).__copilotHost = this;
  }

  /**
   * The `user` callback: parse the envelope and either fire a transient notice
   * (returning immediately) or open a modal and await the user's answer.
   */
  ask(json: string): Promise<string> {
    let env: Envelope;
    try {
      env = JSON.parse(json) as Envelope;
    } catch {
      // A malformed envelope can't be answered; unblock the core safely.
      return Promise.resolve('{}');
    }

    if (env.kind === 'notify') {
      // MODEL text: normalized at the display boundary in showNotice.
      this.showNotice(env.message, 2500);
      // One-way: the return is ignored, so don't block.
      return Promise.resolve('{}');
    }

    // Never overwrite a live resolver. If a question is already pending (e.g. a
    // second copilot leaked past the run guard), settling this new envelope with
    // `question.set` would drop the first copilot's resolver and hang its wasm
    // pump forever. Answer the newcomer with a safe per-kind skip instead.
    if (this.question()) {
      return Promise.resolve(skipAnswer(env.kind));
    }

    // MODEL text: the prompt is model-phrased and only ever DISPLAYED (never
    // sent back), so normalize it in place. Options are also model-phrased, but
    // the bridge matches the answer by exact string — so normalize only for
    // display and keep the original as the answer value (see PendingQuestion).
    env.prompt = normalizeDashes(env.prompt);
    const choices =
      env.kind === 'select' || env.kind === 'multi_select'
        ? env.options.map((o) => ({ display: normalizeDashes(o), value: o }))
        : [];
    return new Promise<string>((resolve) => this.question.set({ env, resolve, choices }));
  }

  /**
   * Fire an app-global transient notice. Unlike a component-local toast, this
   * renders in {@link CopilotOverlay} at the app root, so it survives a
   * navigation that unmounts the caller (e.g. New Build → tailoring workspace).
   * Held ~4s — a touch longer than a copilot's own `notify` envelopes.
   */
  notify(message: string): void {
    this.showNotice(message, 4000);
  }

  /** Set the notice signal and (re)arm its auto-clear. Shared by the public
   *  {@link notify} and the `ask` path's `{kind:"notify"}` envelopes. */
  private showNotice(message: string, ms: number): void {
    // The single display point for notices (both `notify()` and the ask-path
    // `{kind:"notify"}` envelope). Model-borne notices carry raw dashes, so
    // normalize here (see shared/normalize-dashes).
    this.notice.set(normalizeDashes(message));
    if (this.noticeTimer) clearTimeout(this.noticeTimer);
    this.noticeTimer = setTimeout(() => this.notice.set(null), ms);
  }

  /** `select` answer → the chosen option's exact text. */
  answerSelect(optionText: string): void {
    this.settle(JSON.stringify({ choice: optionText }));
  }

  /** `multi_select` answer → the chosen option texts (empty array = none). */
  answerMulti(optionTexts: string[]): void {
    this.settle(JSON.stringify({ choices: optionTexts }));
  }

  /** `text` answer → the user's text (empty string is a valid skip). */
  answerText(text: string): void {
    this.settle(JSON.stringify({ text }));
  }

  /** `confirm` answer → the boolean. */
  answerConfirm(v: boolean): void {
    this.settle(JSON.stringify({ confirm: v }));
  }

  /**
   * Dismiss the current question with a safe skip so a closed modal never hangs
   * the wasm. Each kind resolves with the field shape the core expects.
   */
  cancel(): void {
    const q = this.question();
    if (!q) return;
    this.settle(skipAnswer(q.env.kind));
  }

  /** Resolve the pending resolver and close the modal. */
  private settle(answer: string): void {
    const q = this.question();
    if (!q) return;
    q.resolve(answer);
    this.question.set(null);
  }

  /** The `on_progress` callback: record the event and refresh the cost. */
  onProgress(json: string): void {
    let ev: ProgressEvent;
    try {
      ev = JSON.parse(json) as ProgressEvent;
    } catch {
      return;
    }
    // MODEL text: the progress line is model-phrased; normalize it before it
    // reaches the overlay (see shared/normalize-dashes).
    if (ev.message) ev.message = normalizeDashes(ev.message);
    this.progress.update((list) => [...list, ev]);

    if (ev.usage) {
      const totals = {
        input: this.usage().input + (ev.usage.input_tokens ?? 0),
        output: this.usage().output + (ev.usage.output_tokens ?? 0),
      };
      this.usage.set(totals);
      this.api.getCost(this.wasm.models.model, totals.input, totals.output).subscribe({
        next: (c) =>
          this.costLabel.set(c.usd != null ? '$' + c.usd.toFixed(2) : (c.subscription_note ?? '')),
        error: () => {},
      });
    }
  }

  /**
   * Run `fn` with the progress overlay showing `label`, resetting the run's
   * progress/usage/cost first and always tearing the UI down afterwards.
   *
   * `opts.cancellable` marks the run as stoppable, so the overlay offers a Stop
   * button wired to {@link requestStop} (only the tailor loop honors it today).
   */
  async runWithUi<T>(
    label: string,
    fn: () => Promise<T>,
    opts?: { cancellable?: boolean },
  ): Promise<T> {
    // Refuse a concurrent run: two copilots sharing one modal would race for the
    // single pending-question slot (see `ask`). Callers catch and toast this.
    if (this.running()) {
      throw new Error('A copilot is already running: finish or dismiss it first.');
    }
    this.running.set(label);
    this.cancellable.set(opts?.cancellable ?? false);
    this.stopping.set(false);
    this.progress.set([]);
    this.usage.set({ input: 0, output: 0 });
    this.costLabel.set('');
    try {
      // Arm the run's cancel flag here — the single reset point. Clearing it now
      // (before `fn`, which for a build begins with gap analysis) means a Stop
      // pressed anywhere in the run, even before the tailor loop starts, survives
      // to the loop's first check instead of being wiped by a wasm-side reset.
      // Inside the try: this awaits the wasm load, and a load failure must tear
      // the run state down via the finally, not strand a permanent overlay.
      if (opts?.cancellable) await this.wasm.resetTailorLoopCancel();
      return await fn();
    } finally {
      this.running.set(null);
      this.cancellable.set(false);
      this.stopping.set(false);
      this.question.set(null);
    }
  }

  /**
   * Ask the in-flight loop to stop after its current pass. Flips `stopping` (so
   * the overlay swaps the Stop button for a "stopping…" note) and signals the
   * wasm loop's cancel flag; the loop finishes the pass in flight and returns
   * the best draft seen, which the caller saves as usual.
   */
  requestStop(): void {
    if (!this.cancellable() || this.stopping()) return;
    this.stopping.set(true);
    void this.wasm.cancelTailorLoop();
  }

  /**
   * Retire the Stop affordance once the cancellable phase is over — the loop
   * has returned and nothing left in the run (projectHuman, save) can honor a
   * stop. The caller invokes this the moment the loop resolves, so the overlay
   * doesn't keep offering a Stop that can no longer do anything.
   */
  endCancellable(): void {
    this.cancellable.set(false);
    this.stopping.set(false);
  }
}

/** The safe "skip" answer for a pending question, by kind — the shape the core
 *  expects so a dismissed/superseded prompt resolves instead of hanging. Shared
 *  by `cancel()` (user closed the modal) and `ask()` (a second question arrived
 *  while one was live). */
function skipAnswer(kind: QuestionEnvelope['kind']): string {
  switch (kind) {
    case 'confirm':
      return JSON.stringify({ confirm: false });
    // A dismissed choice modal is a deliberate bail-out, not "decline everything":
    // resolving `{abort:true}` tells the bridge to end the session cleanly and keep
    // whatever evidence was recorded so far, instead of losing it (`select`) or
    // silently declining every keyword (`multi_select`).
    case 'multi_select':
    case 'select':
      return JSON.stringify({ abort: true });
    default:
      return JSON.stringify({ text: '' });
  }
}

/**
 * Renders the {@link CopilotHost} state as a fixed overlay above the whole app:
 * a modal Q&A dialog while a copilot is waiting on an answer, a transient toast
 * for one-way notices, and an unobtrusive corner panel with live progress and
 * cost while an operation runs. Everything is hidden when its signal is null.
 */
@Component({
  selector: 'app-copilot-overlay',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (host.question(); as q) {
      <div
        class="scrim"
        [attr.title]="isChoice(q.env.kind) ? 'Click outside or press Escape to end this session' : null"
        (click)="host.cancel()"
      ></div>
      <div
        #dialog
        class="modal"
        role="dialog"
        aria-modal="true"
        [attr.aria-label]="q.env.prompt"
        [attr.aria-description]="isChoice(q.env.kind) ? 'Press Escape or click outside to end this session.' : null"
        (keydown)="onKeydown($event)"
      >
        <p class="prompt">{{ q.env.prompt }}</p>

        @switch (q.env.kind) {
          @case ('select') {
            <div class="opts">
              @for (c of q.choices; track c.value) {
                <button class="opt" type="button" (click)="host.answerSelect(c.value)">
                  {{ c.display }}
                </button>
              }
            </div>
            <div class="foot bail">
              <button class="btn btn-ghost end" type="button" (click)="host.cancel()">
                End session
              </button>
            </div>
          }
          @case ('multi_select') {
            <div class="opts">
              @for (c of q.choices; track $index) {
                <label class="check">
                  <input
                    type="checkbox"
                    [checked]="checked().has($index)"
                    (change)="toggle($index)"
                  />
                  <span>{{ c.display }}</span>
                </label>
              }
            </div>
            <div class="foot">
              <button class="btn btn-ghost end" type="button" (click)="host.cancel()">
                End session
              </button>
              <button class="btn primary" type="button" (click)="submitMulti(q.choices)">
                Done
              </button>
            </div>
          }
          @case ('text') {
            <textarea
              #firstText
              class="ta"
              rows="4"
              [value]="draftText()"
              (input)="draftText.set(asValue($event))"
            ></textarea>
            <div class="foot">
              <button class="btn btn-ghost" type="button" (click)="host.answerText('')">Skip</button>
              <button class="btn primary" type="button" (click)="host.answerText(draftText())">
                Submit
              </button>
            </div>
          }
          @case ('confirm') {
            <div class="foot confirm">
              <button
                class="btn"
                [class.primary]="!q.env.default"
                type="button"
                (click)="host.answerConfirm(false)"
              >
                No
              </button>
              <button
                class="btn"
                [class.primary]="q.env.default"
                type="button"
                (click)="host.answerConfirm(true)"
              >
                Yes
              </button>
            </div>
          }
        }
      </div>
    }

    @if (host.notice(); as msg) {
      <div class="notice" role="status">{{ msg }}</div>
    }

    @if (host.running(); as label) {
      <div class="progress" role="status" aria-live="polite">
        <div class="prog-head">
          <span class="spinner" aria-hidden="true"></span>
          <span class="prog-label">{{ label }}</span>
        </div>
        @if (latest(); as ev) {
          <div class="prog-line">
            <span>{{ ev.message || ev.phase }}</span>
            @if (ev.iteration != null) {
              <span class="dim">· iter {{ ev.iteration }}</span>
            }
            @if (ev.score != null) {
              <span class="dim">· score {{ ev.score }}</span>
            }
          </div>
        }
        @if (host.costLabel()) {
          <div class="cost">· {{ host.costLabel() }}</div>
        }
        @if (host.cancellable()) {
          @if (host.stopping()) {
            <div class="stopping">Stopping after the current pass…</div>
          } @else {
            <button class="btn stop" type="button" (click)="host.requestStop()">Stop</button>
          }
        }
      </div>
    }
  `,
  styles: `
    :host {
      display: contents;
    }

    /* ── Q&A modal ─────────────────────────────────────────────────────── */
    .scrim {
      position: fixed;
      inset: 0;
      z-index: 90;
      background: color-mix(in oklch, var(--fg) 32%, transparent);
      backdrop-filter: blur(2px);
    }
    .modal {
      position: fixed;
      z-index: 100;
      top: 50%;
      left: 50%;
      transform: translate(-50%, -50%);
      width: 460px;
      max-width: 94vw;
      max-height: 88vh;
      overflow-y: auto;
      display: flex;
      flex-direction: column;
      gap: 16px;
      padding: 22px 24px;
      background: var(--bg);
      border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      box-shadow: 0 24px 60px -28px color-mix(in oklch, var(--fg) 55%, transparent);
    }
    .prompt {
      font-family: var(--font-display);
      font-size: 16px;
      line-height: 1.45;
      letter-spacing: -0.01em;
      margin: 0;
    }
    .opts {
      display: flex;
      flex-direction: column;
      gap: 8px;
    }
    .opt {
      text-align: left;
      padding: 11px 13px;
      border-radius: var(--radius);
      border: 1px solid var(--border);
      background: var(--surface);
      font: inherit;
      font-size: 14px;
      color: inherit;
      cursor: pointer;
    }
    .opt:hover {
      border-color: var(--fg);
      background: var(--surface-2);
    }
    .check {
      display: flex;
      align-items: flex-start;
      gap: 9px;
      padding: 9px 11px;
      border-radius: var(--radius);
      border: 1px solid var(--border);
      background: var(--surface);
      font-size: 14px;
      line-height: 1.4;
      cursor: pointer;
    }
    .check input {
      margin-top: 2px;
      accent-color: var(--accent);
    }
    .ta {
      width: 100%;
      box-sizing: border-box;
      resize: vertical;
      padding: 10px 12px;
      border-radius: var(--radius);
      border: 1px solid var(--border);
      background: var(--surface);
      font: inherit;
      font-size: 14px;
      line-height: 1.5;
      color: inherit;
    }
    .foot {
      display: flex;
      align-items: center;
      justify-content: flex-end;
      gap: 10px;
    }
    .foot.confirm {
      justify-content: flex-end;
    }
    /* The bail-out row for a single select has no other action, so the
       End session link sits on its own, quietly, to the right. */
    .foot.bail {
      margin-top: -4px;
    }
    .btn.end {
      font-size: 13px;
      color: var(--muted);
    }
    .btn.end:hover {
      color: var(--fg);
    }
    .btn {
      display: inline-flex;
      align-items: center;
      height: 34px;
      padding: 0 16px;
      border-radius: var(--radius);
      border: 1px solid var(--border);
      background: var(--surface);
      font: inherit;
      font-size: 14px;
      color: inherit;
      cursor: pointer;
    }
    .btn:hover {
      border-color: var(--fg);
    }
    .btn-ghost {
      border-color: transparent;
      background: transparent;
    }
    .btn.primary {
      background: var(--accent);
      border-color: var(--accent);
      color: var(--bg);
    }
    .btn.primary:hover {
      filter: brightness(1.05);
    }
    .opt:focus-visible,
    .btn:focus-visible,
    .ta:focus-visible,
    .check:focus-within {
      outline: 2px solid var(--accent);
      outline-offset: 2px;
    }

    /* ── transient notice toast ────────────────────────────────────────── */
    .notice {
      position: fixed;
      z-index: 100;
      left: 50%;
      bottom: 28px;
      transform: translateX(-50%);
      max-width: 90vw;
      padding: 10px 16px;
      border-radius: var(--radius);
      background: var(--fg);
      color: var(--bg);
      font-size: 13px;
      line-height: 1.4;
      box-shadow: 0 12px 30px -16px color-mix(in oklch, var(--fg) 60%, transparent);
    }

    /* ── live progress panel ───────────────────────────────────────────── */
    .progress {
      position: fixed;
      z-index: 80;
      right: 20px;
      bottom: 20px;
      min-width: 220px;
      max-width: 340px;
      display: flex;
      flex-direction: column;
      gap: 6px;
      padding: 12px 14px;
      background: var(--bg);
      border: 1px solid var(--border);
      border-radius: var(--radius);
      box-shadow: 0 16px 40px -24px color-mix(in oklch, var(--fg) 55%, transparent);
    }
    .prog-head {
      display: flex;
      align-items: center;
      gap: 9px;
    }
    .prog-label {
      font-family: var(--font-mono);
      font-size: 11px;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
    }
    .spinner {
      width: 12px;
      height: 12px;
      flex-shrink: 0;
      border-radius: 50%;
      border: 2px solid var(--border);
      border-top-color: var(--accent);
      animation: cp-spin 0.8s linear infinite;
    }
    .prog-line {
      font-size: 13px;
      line-height: 1.4;
      color: var(--fg);
    }
    .prog-line .dim {
      color: var(--muted);
    }
    .cost {
      font-family: var(--font-mono);
      font-size: 12px;
      color: var(--muted);
    }
    .stop {
      align-self: flex-start;
      height: 28px;
      margin-top: 2px;
      padding: 0 12px;
      font-size: 13px;
    }
    .stopping {
      font-size: 12px;
      color: var(--muted);
      font-style: italic;
    }
    @keyframes cp-spin {
      to {
        transform: rotate(360deg);
      }
    }
    /* No spinner animation when the user prefers reduced motion. */
    @media (prefers-reduced-motion: reduce) {
      .spinner {
        animation: none;
        border-top-color: var(--border);
      }
    }
  `,
})
export class CopilotOverlay {
  protected readonly host = inject(CopilotHost);

  private readonly dialog = viewChild<ElementRef<HTMLElement>>('dialog');

  /** Checked indices for a `multi_select`, reset on each new question. */
  protected readonly checked = signal<Set<number>>(new Set());
  /** Working text for a `text` prompt, reset on each new question. */
  protected readonly draftText = signal('');

  /** The most recent progress event, for the panel's one-line status. */
  protected readonly latest = computed<ProgressEvent | undefined>(() => {
    const list = this.host.progress();
    return list.length ? list[list.length - 1] : undefined;
  });

  private opener: HTMLElement | null = null;

  constructor() {
    effect(() => {
      const q = this.host.question();
      // Reset per-question local state whenever the question changes.
      this.checked.set(new Set());
      this.draftText.set('');
      if (q) {
        // Remember the trigger so focus can return to it on close, then focus
        // the first control in the dialog (a light focus trap on open).
        this.opener = document.activeElement as HTMLElement | null;
        queueMicrotask(() => {
          const root = this.dialog()?.nativeElement;
          const first = root?.querySelector<HTMLElement>(
            'button, textarea, input, [tabindex]:not([tabindex="-1"])',
          );
          first?.focus();
        });
      } else if (this.opener) {
        this.opener.focus();
        this.opener = null;
      }
    });
  }

  /** True for the choice kinds whose dismissal ends the whole session (rather
   *  than being a safe per-prompt skip), so the overlay can label the bail-out. */
  protected isChoice(kind: QuestionEnvelope['kind']): boolean {
    return kind === 'select' || kind === 'multi_select';
  }

  /** Read a form control's current value off an input event. */
  protected asValue(ev: Event): string {
    return (ev.target as HTMLInputElement | HTMLTextAreaElement).value;
  }

  protected toggle(i: number): void {
    this.checked.update((s) => {
      const next = new Set(s);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }

  protected submitMulti(choices: { display: string; value: string }[]): void {
    // Answer with the ORIGINAL option strings (`value`), never the normalized
    // `display` — the bridge matches the chosen keywords by exact string.
    const chosen = choices.filter((_, i) => this.checked().has(i)).map((c) => c.value);
    this.host.answerMulti(chosen);
  }

  protected onKeydown(ev: KeyboardEvent): void {
    if (ev.key === 'Escape') {
      ev.preventDefault();
      this.host.cancel();
      return;
    }
    if (ev.key !== 'Tab') return;
    // Focus trap: cycle within the dialog's focusable elements.
    const root = this.dialog()?.nativeElement;
    if (!root) return;
    const items = Array.from(
      root.querySelectorAll<HTMLElement>(
        'button:not([disabled]), textarea, input, a[href], [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => el.offsetParent !== null);
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement as HTMLElement;
    if (ev.shiftKey && active === first) {
      ev.preventDefault();
      last.focus();
    } else if (!ev.shiftKey && active === last) {
      ev.preventDefault();
      first.focus();
    }
  }
}
