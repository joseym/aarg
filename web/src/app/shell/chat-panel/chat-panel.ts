import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';

import { ChatStore } from '../../services/chat-store';
import { WasmService } from '../../services/wasm.service';

/** The min/max the resize handle clamps the panel width to (px). Below the min
 *  the transcript squishes; above the max it starves the workspace column. */
const MIN_WIDTH = 300;
const MAX_WIDTH = 640;

/** One rendered line of the conversation. `error` is a failed turn's message —
 *  shown in the transcript but never sent back as history, so the wire never
 *  ends on an orphaned user turn. */
interface ChatMessage {
  kind: 'user' | 'assistant' | 'error';
  text: string;
}

/** The chat panel: a third column between the build menu and the workspace that
 *  holds a grounded, streaming conversation about the open build. Layout (the
 *  three-column reflow, the resize, the mobile overlay) is driven by the shell;
 *  this component owns the transcript, the streaming render, and the `chat_reply`
 *  dispatch. `:host { display: contents }` so `.chat-rail` is the real grid cell,
 *  mirroring the sidebar. */
@Component({
  selector: 'app-chat-panel',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { '[class.open]': 'open()' },
  template: `
    <aside class="chat-rail" [class.open]="open()">
      <div class="chat-inner" [style.width.px]="width()">
        <div class="ch-head">
          <div class="ch-head-l">
            <div class="ch-kicker"><span class="dot"></span> Advisor</div>
            <div class="ch-title">{{ store.context()?.title || 'Chat' }}</div>
          </div>
          <button class="ch-x" type="button" aria-label="Close chat" (click)="close.emit()">✕</button>
        </div>

        <div #scroll class="ch-scroll">
          @if (messages().length === 0 && pending() === null) {
            <div class="ch-empty">
              @if (store.context()) {
                <p class="ce-lead">Ask about this posting, or how to talk about a bullet in an interview.</p>
                <p class="ce-sub">The advisor sees your recorded background and the draft that shipped. It won't claim experience you haven't recorded.</p>
              } @else {
                <p class="ce-lead">Open a build to chat about it.</p>
                <p class="ce-sub">Pick a build from the list, then ask about its posting or its draft.</p>
              }
            </div>
          } @else {
            <div class="ch-list">
              @for (m of messages(); track $index) {
                <div class="ch-msg" [attr.data-kind]="m.kind">
                  @if (m.kind === 'error') {
                    <div class="ch-bubble err">
                      <span class="err-label">Couldn't reach the model</span>
                      {{ m.text }}
                    </div>
                  } @else {
                    <div class="ch-bubble">{{ m.text }}</div>
                  }
                </div>
              }
              @if (pending() !== null) {
                <div class="ch-msg" data-kind="assistant">
                  <div class="ch-bubble">
                    @if (pending()) {
                      {{ pending() }}<span class="caret" aria-hidden="true"></span>
                    } @else {
                      <span class="typing" aria-label="Thinking"><i></i><i></i><i></i></span>
                    }
                  </div>
                </div>
              }
            </div>
          }
        </div>

        <div class="ch-compose">
          <textarea
            #input
            class="ch-input"
            rows="2"
            [value]="draft()"
            [disabled]="busy() || !store.context()"
            [attr.placeholder]="store.context() ? 'Ask about this build…' : 'Open a build to chat'"
            (input)="draft.set(asValue($event))"
            (keydown)="onKeydown($event)"
          ></textarea>
          <button
            class="ch-send"
            type="button"
            [disabled]="busy() || !canSend()"
            aria-label="Send message"
            (click)="send()"
          >
            @if (busy()) {
              <span class="ch-spin" aria-hidden="true"></span>
            } @else {
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                <path d="M4 12l16-8-6 16-3-6-7-2z" />
              </svg>
            }
          </button>
        </div>
      </div>

      <!-- Right-edge drag handle: desktop-only resize (the mobile overlay is
           full-width, so it has nothing to drag). -->
      <div
        class="ch-resize"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize chat panel"
        (pointerdown)="onResizeStart($event)"
      ></div>
    </aside>
  `,
  styles: `
    :host { display: contents; }

    .chat-rail {
      position: sticky;
      top: 60px;
      height: calc(100vh - 60px);
      border-right: 1px solid var(--border);
      background: color-mix(in oklch, var(--bg) 74%, var(--surface-2));
      overflow: hidden;
    }

    /* Fixed inner width (driven by the panel's current width) so the transcript
     * never reflows while the shell's grid column animates open or closed — it
     * is simply clipped by .chat-rail's overflow, the same trick the sidebar
     * uses for its collapse animation. */
    .chat-inner {
      display: flex;
      flex-direction: column;
      height: 100%;
      min-width: 300px;
    }

    .ch-head {
      display: flex; align-items: flex-start; justify-content: space-between; gap: 10px;
      padding: 16px 16px 12px; border-bottom: 1px solid var(--border);
    }
    .ch-kicker {
      display: flex; align-items: center; gap: 7px;
      font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.12em;
      text-transform: uppercase; color: var(--accent);
    }
    .ch-kicker .dot { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); }
    .ch-title {
      font-family: var(--font-display); font-size: 16px; line-height: 1.2; margin-top: 5px;
      overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 26ch;
    }
    .ch-x {
      flex-shrink: 0; width: 30px; height: 30px; border-radius: 8px;
      border: 1px solid var(--border); background: var(--surface); color: var(--muted);
      cursor: pointer; font-size: 13px; line-height: 1;
      transition: border-color 0.14s, color 0.14s;
    }
    .ch-x:hover { border-color: var(--fg); color: var(--fg); }
    .ch-x:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }

    .ch-scroll { flex: 1; overflow-y: auto; padding: 16px; }

    .ch-empty { display: flex; flex-direction: column; gap: 10px; padding: 28px 8px; color: var(--muted); }
    .ce-lead { font-family: var(--font-display); font-size: 15.5px; line-height: 1.35; color: var(--fg); margin: 0; }
    .ce-sub { font-size: 12.5px; line-height: 1.5; color: var(--faint); margin: 0; }

    .ch-list { display: flex; flex-direction: column; gap: 12px; }
    .ch-msg { display: flex; }
    .ch-msg[data-kind='user'] { justify-content: flex-end; }
    .ch-msg[data-kind='assistant'], .ch-msg[data-kind='error'] { justify-content: flex-start; }

    .ch-bubble {
      max-width: 86%; padding: 10px 13px; border-radius: 14px;
      font-size: 13.5px; line-height: 1.5; white-space: pre-wrap; word-break: break-word;
      border: 1px solid var(--border); background: var(--surface);
    }
    .ch-msg[data-kind='user'] .ch-bubble {
      background: var(--accent); color: oklch(97% 0.02 40); border-color: var(--accent);
      border-bottom-right-radius: 5px;
    }
    .ch-msg[data-kind='assistant'] .ch-bubble { border-bottom-left-radius: 5px; }
    .ch-bubble.err {
      display: flex; flex-direction: column; gap: 4px;
      border-color: color-mix(in oklch, var(--danger) 45%, var(--border));
      background: color-mix(in oklch, var(--danger) 8%, var(--surface));
      color: var(--fg);
    }
    .err-label {
      font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.08em;
      text-transform: uppercase; color: var(--danger);
    }

    /* Blinking caret trailing the streamed text while a reply is in flight. */
    .caret {
      display: inline-block; width: 2px; height: 1em; margin-left: 1px;
      vertical-align: text-bottom; background: currentColor; opacity: 0.55;
      animation: ch-blink 1s step-end infinite;
    }
    .typing { display: inline-flex; gap: 4px; padding: 2px 0; }
    .typing i {
      width: 6px; height: 6px; border-radius: 50%; background: var(--muted);
      animation: ch-bounce 1.2s ease-in-out infinite;
    }
    .typing i:nth-child(2) { animation-delay: 0.15s; }
    .typing i:nth-child(3) { animation-delay: 0.3s; }

    .ch-compose {
      display: flex; align-items: flex-end; gap: 8px;
      padding: 12px 14px 14px; border-top: 1px solid var(--border);
    }
    .ch-input {
      flex: 1; min-width: 0; box-sizing: border-box; resize: none;
      padding: 9px 11px; border-radius: 11px; border: 1px solid var(--border);
      background: var(--surface); font: inherit; font-size: 13.5px; line-height: 1.45;
      color: inherit; max-height: 160px;
    }
    .ch-input:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
    .ch-input:disabled { opacity: 0.6; }
    .ch-send {
      flex-shrink: 0; width: 38px; height: 38px; border-radius: 10px;
      display: inline-flex; align-items: center; justify-content: center;
      border: 1px solid var(--accent); background: var(--accent); color: oklch(97% 0.02 40);
      cursor: pointer; transition: filter 0.14s, opacity 0.14s;
    }
    .ch-send:hover:not(:disabled) { filter: brightness(1.06); }
    .ch-send:disabled { opacity: 0.45; cursor: default; }
    .ch-send:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .ch-send svg { width: 17px; height: 17px; }
    .ch-spin {
      width: 15px; height: 15px; border-radius: 50%;
      border: 2px solid color-mix(in oklch, oklch(97% 0.02 40) 45%, transparent);
      border-top-color: oklch(97% 0.02 40); animation: ch-spin 0.8s linear infinite;
    }

    /* Right-edge drag handle. A 8px hit strip with a hairline that lights on
     * hover; only a real affordance on desktop (the mobile overlay is full-width). */
    .ch-resize {
      position: absolute; top: 0; right: -3px; z-index: 2;
      width: 8px; height: 100%; cursor: col-resize;
      touch-action: none;
    }
    .ch-resize::after {
      content: ''; position: absolute; top: 0; left: 3px;
      width: 2px; height: 100%; background: transparent; transition: background 0.14s;
    }
    .ch-resize:hover::after { background: color-mix(in oklch, var(--accent) 60%, transparent); }

    /* When closed the shell clips the column to 0; take the content out of the
     * focus order and drop the drag handle so nothing clipped stays reachable. */
    .chat-rail:not(.open) .chat-inner { visibility: hidden; }
    .chat-rail:not(.open) .ch-resize { display: none; }

    @keyframes ch-spin { to { transform: rotate(360deg); } }
    @keyframes ch-blink { 0%, 100% { opacity: 0.55; } 50% { opacity: 0; } }
    @keyframes ch-bounce {
      0%, 80%, 100% { transform: translateY(0); opacity: 0.5; }
      40% { transform: translateY(-4px); opacity: 1; }
    }
    @media (prefers-reduced-motion: reduce) {
      .ch-spin, .caret, .typing i { animation: none !important; }
    }

    /* ≤1080px: the workspace can't afford a third column, so the panel overlays
     * full-width from the left, matching the sidebar drawer's breakpoint. The
     * dismiss scrim lives in the shell (like the nav-scrim). */
    @media (max-width: 1080px) {
      .chat-rail {
        position: fixed; top: 0; left: 0; height: 100dvh;
        width: 100vw; z-index: 44;
        transform: translateX(-100%);
        transition: transform 0.28s cubic-bezier(0.2, 0.7, 0.2, 1);
        border-right: 1px solid var(--border);
        box-shadow: 18px 0 50px -30px color-mix(in oklch, var(--fg) 60%, transparent);
      }
      .chat-rail.open { transform: none; }
      .chat-inner { width: 100% !important; min-width: 0; }
      .ch-resize { display: none; }
    }
    @media (max-width: 1080px) and (prefers-reduced-motion: reduce) {
      .chat-rail { transition: none; }
    }
  `,
})
export class ChatPanel {
  protected readonly store = inject(ChatStore);
  private readonly wasm = inject(WasmService);

  /** Whether the panel is shown. When false the shell collapses its grid column
   *  to 0 and clips this content; kept mounted so the transcript survives a
   *  close/reopen. */
  readonly open = input(false);
  /** The panel's current width in px (desktop). The shell owns the value and the
   *  persistence; this drives the fixed inner width and the resize math. */
  readonly width = input(MIN_WIDTH);
  readonly close = output<void>();
  /** Emitted continuously while the handle is dragged, with the new clamped width. */
  readonly widthChange = output<number>();

  private readonly scroll = viewChild<ElementRef<HTMLElement>>('scroll');
  private readonly inputEl = viewChild<ElementRef<HTMLTextAreaElement>>('input');

  /** The displayed transcript, including errored turns. */
  protected readonly messages = signal<ChatMessage[]>([]);
  /** The in-flight assistant reply text, or null when no turn is streaming.
   *  Empty string means "started, no delta yet" (renders the typing dots). */
  protected readonly pending = signal<string | null>(null);
  /** True while a turn is in flight; disables the input and send button. */
  protected readonly busy = signal(false);
  /** The compose box's current text. */
  protected readonly draft = signal('');

  /** The completed turns sent back as `chat_reply` history — only whole
   *  [user, assistant] pairs, so an errored turn never leaves an orphaned user
   *  turn on the wire. */
  private wireHistory: { from_user: boolean; text: string }[] = [];
  /** The build the current transcript belongs to, so a build switch resets it. */
  private currentBuildId: string | null = null;

  constructor() {
    // Reset the conversation when the open build changes (or the context clears):
    // the transcript, history, and any in-flight state all belong to one build.
    effect(() => {
      const id = this.store.context()?.buildId ?? null;
      if (id !== this.currentBuildId) {
        this.currentBuildId = id;
        this.messages.set([]);
        this.wireHistory = [];
        this.pending.set(null);
        this.busy.set(false);
      }
    });

    // Autoscroll to the newest content whenever the transcript or the streaming
    // reply grows.
    effect(() => {
      this.messages();
      this.pending();
      const el = this.scroll()?.nativeElement;
      if (el) requestAnimationFrame(() => (el.scrollTop = el.scrollHeight));
    });
  }

  protected asValue(ev: Event): string {
    return (ev.target as HTMLTextAreaElement).value;
  }

  /** True when there's a non-blank draft and a build to talk about. */
  protected canSend(): boolean {
    return this.draft().trim().length > 0 && this.store.context() !== null;
  }

  protected onKeydown(ev: KeyboardEvent): void {
    // Enter sends; Shift+Enter inserts a newline (the textarea default).
    if (ev.key === 'Enter' && !ev.shiftKey) {
      ev.preventDefault();
      void this.send();
    }
  }

  /** Send the current draft as one chat turn, streaming the reply into the
   *  transcript. On failure the error shows in the transcript, the draft is
   *  restored so nothing typed is lost, and the wire history is left untouched
   *  (so the next send doesn't carry a dangling user turn). */
  protected async send(): Promise<void> {
    const ctx = this.store.context();
    const message = this.draft().trim();
    if (!message || this.busy() || !ctx) return;

    this.draft.set('');
    this.messages.update((m) => [...m, { kind: 'user', text: message }]);
    this.busy.set(true);
    this.pending.set('');
    const history = this.wireHistory;

    try {
      const reply = await this.wasm.chatReply(
        {
          dataset: ctx.dataset,
          jd: ctx.jd,
          canonical: ctx.canonical,
          report: ctx.report,
          transcript: history,
          message,
        },
        (delta) => this.pending.update((p) => (p ?? '') + delta),
      );
      this.messages.update((m) => [...m, { kind: 'assistant', text: reply }]);
      this.wireHistory = [
        ...history,
        { from_user: true, text: message },
        { from_user: false, text: reply },
      ];
    } catch (err) {
      this.messages.update((m) => [...m, { kind: 'error', text: errMessage(err) }]);
      // Restore the unsent draft so a failed turn loses nothing.
      this.draft.set(message);
    } finally {
      this.pending.set(null);
      this.busy.set(false);
      // Return focus to the compose box for the next turn.
      queueMicrotask(() => this.inputEl()?.nativeElement.focus());
    }
  }

  // ── resize ────────────────────────────────────────────────────────────
  private resizeStartX = 0;
  private resizeStartWidth = 0;

  protected onResizeStart(ev: PointerEvent): void {
    ev.preventDefault();
    this.resizeStartX = ev.clientX;
    this.resizeStartWidth = this.width();
    const handle = ev.currentTarget as HTMLElement;
    handle.setPointerCapture(ev.pointerId);
    handle.addEventListener('pointermove', this.onResizeMove);
    const end = (): void => {
      handle.removeEventListener('pointermove', this.onResizeMove);
      handle.removeEventListener('pointerup', end);
      handle.removeEventListener('pointercancel', end);
    };
    handle.addEventListener('pointerup', end);
    handle.addEventListener('pointercancel', end);
  }

  private readonly onResizeMove = (ev: PointerEvent): void => {
    const next = clamp(this.resizeStartWidth + (ev.clientX - this.resizeStartX), MIN_WIDTH, MAX_WIDTH);
    this.widthChange.emit(next);
  };
}

/** Clamp `n` into `[lo, hi]`. */
function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

/** The human message behind a thrown value — the server envelope's message, a
 *  wasm reject's plain string, or an `Error`. Mirrors the workspace's own
 *  unwrap so a failed turn reads the same clean line the toasts do. */
function errMessage(err: unknown): string {
  if (err instanceof HttpErrorResponse) {
    const b = err.error as { error?: { message?: string }; message?: string } | string | null;
    if (typeof b === 'string') return b || err.message;
    return b?.error?.message ?? b?.message ?? err.message;
  }
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  return 'the request failed';
}
