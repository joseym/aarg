import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
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
import { ArtifactCard } from './artifact-card';
import {
  type ArtifactKind,
  type ReplySegment,
  isSlashDraft,
  matchSlashCommand,
  parseReply,
  slashSuggestions,
  stripMarkers,
} from './artifacts';
import { renderChatMarkdown } from './markdown';

/** The min/max the resize handle clamps the panel width to (px). Below the min
 *  the transcript squishes; above the max it starves the workspace column. */
const MIN_WIDTH = 300;
const MAX_WIDTH = 640;

/** One rendered line of the conversation.
 *  - `user`/`error` carry plain `text`. `user` renders it as markdown (what
 *    the person typed, styled the same as a reply); `error` is a failed
 *    turn's message and stays literal. Neither is sent back as history for
 *    an errored turn, so the wire never ends on an orphaned user turn.
 *  - `assistant` carries `segments`: the reply parsed into prose runs and any
 *    artifact cards the model attached with a marker, in order.
 *  - `card` is a standalone artifact a slash command inserted client-side (no
 *    model call), never part of the wire history. */
interface ChatMessage {
  kind: 'user' | 'assistant' | 'error' | 'card';
  text?: string;
  segments?: ReplySegment[];
  artifact?: ArtifactKind;
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
  imports: [ArtifactCard],
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
                @switch (m.kind) {
                  @case ('error') {
                    <div class="ch-msg" data-kind="error">
                      <div class="ch-bubble err">
                        <span class="err-label">Couldn't reach the model</span>
                        {{ m.text }}
                      </div>
                    </div>
                  }
                  @case ('assistant') {
                    @for (seg of m.segments; track $index) {
                      @if (asText(seg); as t) {
                        <div class="ch-msg" data-kind="assistant">
                          <div class="ch-bubble md" [innerHTML]="renderMarkdown(t)"></div>
                        </div>
                      } @else {
                        <div class="ch-msg" data-kind="artifact">
                          <app-artifact-card [artifact]="asArtifact(seg)" />
                        </div>
                      }
                    }
                  }
                  @case ('card') {
                    <div class="ch-msg" data-kind="artifact">
                      <app-artifact-card [artifact]="m.artifact!" />
                    </div>
                  }
                  @default {
                    <div class="ch-msg" data-kind="user">
                      <div class="ch-bubble md" [innerHTML]="renderMarkdown(m.text!)"></div>
                    </div>
                  }
                }
              }
              @if (pending() !== null) {
                <div class="ch-msg" data-kind="assistant">
                  <div class="ch-bubble">
                    @if (streamedText()) {
                      {{ streamedText() }}<span class="caret" aria-hidden="true"></span>
                    } @else {
                      <span class="typing" aria-label="Thinking"><i></i><i></i><i></i></span>
                    }
                  </div>
                </div>
              }
            </div>
          }
        </div>

        @if (suggestions().length > 0) {
          <div class="ch-slash" role="listbox" aria-label="Slash commands">
            @for (c of suggestions(); track c.name) {
              <button
                class="ch-slash-row"
                type="button"
                role="option"
                (click)="applySuggestion(c.name)"
              >
                <span class="ch-slash-name">/{{ c.name }}</span>
                <span class="ch-slash-hint">{{ c.hint }}</span>
              </button>
            }
          </div>
        }

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
    /* An artifact card spans the transcript width rather than sitting in a
     * bubble; it is its own titled element. */
    .ch-msg[data-kind='artifact'] { display: block; }
    .ch-msg[data-kind='artifact'] app-artifact-card { display: block; width: 100%; }

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

    /* Markdown-rendered prose (both user and assistant bubbles): the base
     * bubble's pre-wrap white-space is meant for a raw text node (the
     * streaming placeholder) and would otherwise turn the whitespace between
     * marked's block elements into visible blank lines, so real elements take
     * over spacing via margins. Tints use currentColor rather than a fixed
     * color so they read against either bubble's background — the light text
     * on the accent-colored user bubble, or the normal text on the rest. */
    .ch-bubble.md { white-space: normal; }
    .ch-bubble.md :is(p, ul, ol) { margin: 0 0 8px; }
    .ch-bubble.md :is(p, ul, ol):last-child { margin-bottom: 0; }
    .ch-bubble.md ul, .ch-bubble.md ol { padding-left: 20px; }
    .ch-bubble.md li { margin: 2px 0; }
    .ch-bubble.md li > p { margin: 0; }
    .ch-bubble.md strong { font-weight: 600; }
    .ch-bubble.md a { color: var(--accent); text-decoration: underline; }
    .ch-msg[data-kind='user'] .ch-bubble.md a { color: inherit; }
    .ch-bubble.md code {
      font-family: var(--font-mono); font-size: 0.9em;
      background: color-mix(in oklch, currentColor 15%, transparent);
      padding: 1px 4px; border-radius: 4px;
    }
    .ch-bubble.md pre {
      margin: 0 0 8px; padding: 8px 10px; border-radius: 8px; overflow-x: auto;
      background: color-mix(in oklch, currentColor 15%, transparent);
    }
    .ch-bubble.md pre code { background: none; padding: 0; }
    .ch-bubble.md pre:last-child { margin-bottom: 0; }
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

    /* Slash-command autocomplete: a small list that floats just above the
     * compose box while the draft is a slash invocation. */
    .ch-slash {
      display: flex; flex-direction: column;
      margin: 0 14px; border: 1px solid var(--border); border-radius: 10px;
      background: var(--surface); overflow: hidden;
    }
    .ch-slash-row {
      display: flex; align-items: baseline; gap: 10px;
      padding: 7px 11px; border: none; background: none; cursor: pointer;
      text-align: left; color: inherit;
      border-bottom: 1px solid color-mix(in oklch, var(--border) 60%, transparent);
    }
    .ch-slash-row:last-child { border-bottom: none; }
    .ch-slash-row:hover { background: color-mix(in oklch, var(--accent) 8%, var(--surface)); }
    .ch-slash-row:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
    .ch-slash-name {
      font-family: var(--font-mono); font-size: 12px; color: var(--accent); flex-shrink: 0;
    }
    .ch-slash-hint { font-size: 12px; color: var(--muted); }

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
  /** Emitted continuously while the handle is dragged, with the new clamped width
   *  (the shell updates the live column but does not persist). */
  readonly widthChange = output<number>();
  /** Emitted once on pointer release, with the final width, so the shell persists
   *  it exactly once per drag rather than on every move. */
  readonly widthCommit = output<number>();

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

  /** The in-flight reply with any artifact markers stripped, including a
   *  half-arrived trailing one, so a raw sentinel never flashes mid-stream. The
   *  finalized reply is parsed into cards by {@link parseReply} on completion. */
  protected readonly streamedText = computed(() => stripMarkers(this.pending() ?? ''));

  /** The slash commands matching the current draft, for the autocomplete hint.
   *  Empty unless the draft is a `/name` invocation still being typed. */
  protected readonly suggestions = computed(() => slashSuggestions(this.draft()));

  /** The completed turns sent back as `chat_reply` history — only whole
   *  [user, assistant] pairs, so an errored turn never leaves an orphaned user
   *  turn on the wire. */
  private wireHistory: { from_user: boolean; text: string }[] = [];
  /** The build the current transcript belongs to, so a build switch resets it. */
  private currentBuildId: string | null = null;
  /** Monotonic id of the current turn. Bumped on every send and on every build
   *  switch, so an in-flight turn can tell it has been superseded and discard its
   *  own deltas and result instead of bleeding them into a different build. */
  private turnSeq = 0;

  constructor() {
    // Reset the conversation when the open build changes (or the context clears):
    // the transcript, history, and any in-flight state all belong to one build.
    // Bumping the turn id here supersedes any turn still streaming for the old
    // build, so its deltas and result are dropped rather than landing here.
    effect(() => {
      const id = this.store.context()?.buildId ?? null;
      if (id !== this.currentBuildId) {
        this.currentBuildId = id;
        this.turnSeq++;
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

  /** The prose of a reply segment, or null when it is an artifact card. Split
   *  this way so the template renders a bubble for text and a card otherwise. */
  protected asText(seg: ReplySegment): string | null {
    return 'text' in seg ? seg.text : null;
  }

  /** The artifact of a card segment (only called when `asText` returned null). */
  protected asArtifact(seg: ReplySegment): ArtifactKind {
    return (seg as { artifact: ArtifactKind }).artifact;
  }

  /** A finalized user or assistant message as HTML. Bound via `[innerHTML]`,
   *  which Angular sanitizes at bind time — this only needs to produce markup,
   *  not defend against injection. Never used on the in-flight streamed reply
   *  (kept as plain text so it doesn't flash oddly rendered mid-token). */
  protected renderMarkdown(text: string): string {
    return renderChatMarkdown(text);
  }

  /** Run the named slash command straight from the autocomplete list. */
  protected applySuggestion(name: string): void {
    this.draft.set(`/${name}`);
    void this.send();
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
   *  (so the next send doesn't carry a dangling user turn).
   *
   *  A turn id is captured up front; if the build switches mid-flight (which
   *  supersedes this turn via the reset effect) every continuation bails, so a
   *  stale turn never renders its deltas into, appends its answer to, or
   *  overwrites the history of the build now open. */
  protected async send(): Promise<void> {
    const ctx = this.store.context();
    const message = this.draft().trim();
    if (!message || this.busy() || !ctx) return;

    // Slash commands are handled client-side with no model call: a known one
    // inserts its artifact card straight from stored content, an unknown one
    // gets a short hint. Either way the turn never reaches the LLM or the wire.
    if (isSlashDraft(message)) {
      this.runSlashCommand(message);
      return;
    }

    const turn = ++this.turnSeq;
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
        // `text` is the running accumulated reply; only this turn may paint it.
        (text) => {
          if (this.turnSeq === turn) this.pending.set(text);
        },
      );
      if (this.turnSeq !== turn) return; // superseded by a build switch: discard
      // Parse the finalized reply into prose runs and any artifact cards the
      // model attached with a marker. The RAW reply (markers included) is what
      // goes back on the wire, so the model keeps continuity with what it said.
      this.messages.update((m) => [...m, { kind: 'assistant', segments: parseReply(reply) }]);
      this.wireHistory = [
        ...history,
        { from_user: true, text: message },
        { from_user: false, text: reply },
      ];
    } catch (err) {
      if (this.turnSeq !== turn) return; // superseded: don't touch the new build
      this.messages.update((m) => [...m, { kind: 'error', text: errMessage(err) }]);
      // Restore the unsent draft so a failed turn loses nothing.
      this.draft.set(message);
    } finally {
      // Only the owning turn clears the shared in-flight state — a superseded
      // turn must not stomp a fresh turn's pending/busy on the new build.
      if (this.turnSeq === turn) {
        this.pending.set(null);
        this.busy.set(false);
        // Return focus to the compose box for the next turn.
        queueMicrotask(() => this.inputEl()?.nativeElement.focus());
      }
    }
  }

  /** Apply a `/name` draft: insert the matched command's artifact card, or show
   *  a one-line hint for an unknown command. Pure client-side — no model call,
   *  and nothing is added to the wire history. */
  private runSlashCommand(message: string): void {
    const command = matchSlashCommand(message);
    this.draft.set('');
    if (!command) {
      const names = slashSuggestions('/').map((c) => `/${c.name}`);
      this.messages.update((m) => [
        ...m,
        { kind: 'assistant', segments: [{ text: `I don't know that command. Try ${names.join(', ')}.` }] },
      ]);
      return;
    }
    const outcome = command.run();
    // One outcome kind today (`artifact`); a future `/skills` adds its own here.
    this.messages.update((m) => [...m, { kind: 'card', artifact: outcome.artifact }]);
    // Keep focus in the compose box for the next line.
    queueMicrotask(() => this.inputEl()?.nativeElement.focus());
  }

  // ── resize ────────────────────────────────────────────────────────────
  private resizeStartX = 0;
  private resizeStartWidth = 0;
  /** The most recent width during the current drag, committed on release. */
  private lastResizeWidth = 0;

  protected onResizeStart(ev: PointerEvent): void {
    ev.preventDefault();
    this.resizeStartX = ev.clientX;
    this.resizeStartWidth = this.width();
    this.lastResizeWidth = this.width();
    const handle = ev.currentTarget as HTMLElement;
    handle.setPointerCapture(ev.pointerId);
    handle.addEventListener('pointermove', this.onResizeMove);
    const end = (): void => {
      handle.removeEventListener('pointermove', this.onResizeMove);
      handle.removeEventListener('pointerup', end);
      handle.removeEventListener('pointercancel', end);
      // Persist exactly once, at the end of the drag, not on every move.
      this.widthCommit.emit(this.lastResizeWidth);
    };
    handle.addEventListener('pointerup', end);
    handle.addEventListener('pointercancel', end);
  }

  private readonly onResizeMove = (ev: PointerEvent): void => {
    this.lastResizeWidth = clamp(
      this.resizeStartWidth + (ev.clientX - this.resizeStartX),
      MIN_WIDTH,
      MAX_WIDTH,
    );
    this.widthChange.emit(this.lastResizeWidth);
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
