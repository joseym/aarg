import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  effect,
  inject,
  input,
  output,
  signal,
} from '@angular/core';

/** One entry in a build's on-disk edit log (mirrors the workspace's
 *  `EditLogEntry`). Kept structurally identical so the workspace can pass its
 *  own entries through `revert` without a cast. */
export interface EditLogEntry {
  at: string;
  target: string;
  prev: string;
  next: string;
}

/** A pre-formatted row for the history popover. The workspace owns the label /
 *  time / truncation helpers, so it hands the bar ready-to-render strings plus
 *  the original `entry` to emit back on Revert — the bar stays presentational. */
export interface EditLogRow {
  label: string;
  time: string;
  prevText: string;
  nextText: string;
  entry: EditLogEntry;
}

/** The slide-up sticky pending-edits bar: a viewport-anchored action toolbar for
 *  the résumé preview's editing mode. Purely presentational — the workspace owns
 *  the single visibility rule (its `showEditBar` computed) and drives every
 *  signal through inputs; the bar only reflects state and emits intents. It
 *  stays mounted so the slide-in/out CSS transition can play (hidden = opacity 0
 *  + pointer-events none; motion suppressed under prefers-reduced-motion). */
@Component({
  selector: 'app-edit-bar',
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    role: 'toolbar',
    'aria-label': 'Pending edits',
    '[class.visible]': 'visible()',
    '(document:keydown.escape)': 'closeHistory()',
    '(document:click)': 'onDocumentClick($event)',
  },
  template: `
    <div class="left">
      @if (editCount() > 0) {
        <span class="pulse" aria-hidden="true"></span>
        <div class="lt">
          <div class="count"><b>{{ editCount() }}</b> pending edit{{ editCount() === 1 ? '' : 's' }}</div>
          <div class="cap">Save rewrites this build and re-renders the PDFs · Record adds evidence to your dataset.</div>
        </div>
      } @else {
        <span class="pulse saved" aria-hidden="true"></span>
        <div class="lt"><div class="count">All edits saved</div></div>
      }
    </div>

    <div class="right">
      <button class="eb-btn ghost" type="button" (click)="undo.emit()" [disabled]="!canUndo()">Undo ↩</button>
      @if (editCount() > 0) {
        <button class="eb-btn ghost" type="button" (click)="record.emit()" [disabled]="recording()">
          {{ recording() ? 'Recording…' : 'Record in dataset' }}
        </button>
        <button class="eb-btn primary" type="button" (click)="save.emit()" [disabled]="saving()">
          {{ saving() ? 'Saving…' : 'Save to this build' }}
        </button>
      }

      @if (logRows().length > 0) {
        <div class="hist">
          <button
            class="eb-icon"
            type="button"
            aria-label="Edit history"
            [attr.aria-expanded]="historyOpen()"
            (click)="toggleHistory()"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
              <circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" />
            </svg>
            <span class="badge">{{ logRows().length }}</span>
          </button>

          @if (historyOpen()) {
            <div class="pop" role="dialog" aria-label="Edit history">
              <div class="pop-head">Edit history ({{ logRows().length }})</div>
              <ul>
                @for (row of logRows(); track $index) {
                  <li>
                    <div class="el-row">
                      <span class="el-target">{{ row.label }}</span>
                      <span class="el-time">{{ row.time }}</span>
                      <button
                        class="eb-btn ghost el-revert"
                        type="button"
                        (click)="revert.emit(row.entry)"
                        [disabled]="saving()"
                      >
                        Revert
                      </button>
                    </div>
                    <div class="el-diff">
                      <span class="el-prev">{{ row.prevText }}</span>
                      <span class="el-arrow" aria-hidden="true">→</span>
                      <span class="el-next">{{ row.nextText }}</span>
                    </div>
                  </li>
                }
              </ul>
            </div>
          }
        </div>
      }
    </div>
  `,
  styles: `
    :host {
      position: fixed;
      bottom: 18px;
      left: 50%;
      z-index: 70;
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 18px;
      width: min(720px, calc(100% - 32px));
      padding: 12px 14px 12px 18px;
      background: var(--surface);
      border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      box-shadow: 0 8px 24px color-mix(in oklch, var(--fg) 12%, transparent);
      opacity: 0;
      pointer-events: none;
      /* visibility (delayed until after the slide-out) removes the hidden bar
         from the tab order and the accessibility tree — opacity/pointer-events
         only block the mouse, so Tab could still reach and Enter-trigger the
         invisible buttons from coverage/pixel modes. */
      visibility: hidden;
      transform: translate(-50%, 140%);
      transition: transform 180ms ease, opacity 180ms ease, visibility 0s linear 180ms;
    }
    :host(.visible) {
      opacity: 1;
      pointer-events: auto;
      visibility: visible;
      transition-delay: 0s;
      transform: translate(-50%, 0);
    }

    .left { display: flex; align-items: center; gap: 11px; min-width: 0; }
    .pulse { flex: none; width: 9px; height: 9px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 0 0 color-mix(in oklch, var(--accent) 55%, transparent); animation: eb-pulse 1.8s ease-out infinite; }
    .pulse.saved { background: var(--success); animation: none; box-shadow: none; }
    @keyframes eb-pulse {
      0% { box-shadow: 0 0 0 0 color-mix(in oklch, var(--accent) 50%, transparent); }
      70% { box-shadow: 0 0 0 7px transparent; }
      100% { box-shadow: 0 0 0 0 transparent; }
    }
    .lt { min-width: 0; }
    .count { font-size: 14px; color: var(--fg); }
    .count b { font-weight: 700; }
    .cap { margin-top: 2px; font-size: 11.5px; line-height: 1.35; color: var(--muted); }

    .right { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }

    .eb-btn { display: inline-flex; align-items: center; gap: 6px; height: 32px; padding: 0 13px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 13.5px; font-weight: 500; color: inherit; cursor: pointer; }
    .eb-btn:disabled { opacity: 0.55; cursor: default; }
    .eb-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .eb-btn.ghost { border-color: transparent; background: transparent; color: var(--muted); }
    .eb-btn.ghost:hover:not(:disabled) { border-color: var(--border); color: var(--fg); }
    .eb-btn.primary { border-color: var(--accent); background: var(--accent); color: oklch(97% 0.02 40); }
    .eb-btn.primary:hover:not(:disabled) { background: var(--accent-2); border-color: var(--accent-2); }

    .hist { position: relative; display: inline-flex; }
    .eb-icon { position: relative; display: inline-flex; align-items: center; justify-content: center; width: 34px; height: 32px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); color: var(--muted); cursor: pointer; }
    .eb-icon:hover { color: var(--fg); border-color: var(--fg); }
    .eb-icon:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .eb-icon svg { width: 16px; height: 16px; }
    .eb-icon .badge { position: absolute; top: -6px; right: -6px; min-width: 16px; height: 16px; padding: 0 4px; border-radius: 999px; background: var(--accent); color: oklch(97% 0.02 40); font-family: var(--font-mono); font-size: 10px; line-height: 16px; text-align: center; }

    .pop { position: absolute; bottom: calc(100% + 10px); right: 0; z-index: 1; width: 340px; max-width: 78vw; max-height: 40vh; overflow-y: auto; background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius); box-shadow: 0 12px 30px -12px color-mix(in oklch, var(--fg) 45%, transparent); }
    .pop-head { padding: 9px 12px; font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.06em; text-transform: uppercase; color: var(--muted); border-bottom: 1px solid var(--border); position: sticky; top: 0; background: var(--surface); }
    .pop ul { list-style: none; margin: 0; padding: 4px 0; }
    .pop li { padding: 8px 12px; display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; }
    .pop li + li { border-top: 1px solid color-mix(in oklch, var(--border) 55%, transparent); }
    .el-row { display: flex; align-items: center; gap: 10px; }
    .el-target { font-weight: 600; color: var(--fg); }
    .el-time { color: var(--faint); font-family: var(--font-mono); font-size: 11px; }
    .el-revert { margin-left: auto; height: 24px; padding: 0 9px; font-size: 12px; }
    .el-diff { display: flex; align-items: baseline; gap: 7px; color: var(--muted); flex-wrap: wrap; }
    .el-prev { text-decoration: line-through; color: var(--faint); }
    .el-next { color: var(--fg); }
    .el-arrow { color: var(--faint); }

    @media (max-width: 1080px) {
      :host { left: 16px; right: 16px; width: auto; transform: translateY(140%); }
      :host(.visible) { transform: translateY(0); }
    }

    @media (prefers-reduced-motion: reduce) {
      :host { transition: none; }
      .pulse { animation: none; }
    }
  `,
})
export class EditBar {
  /** Whether the bar is slid in (pending edits OR undoable session history). */
  readonly visible = input(false);
  readonly editCount = input(0);
  /** Session undo stack is non-empty. */
  readonly canUndo = input(false);
  readonly recording = input(false);
  readonly saving = input(false);
  /** The build's on-disk edit log, pre-formatted for the history popover. */
  readonly logRows = input<EditLogRow[]>([]);

  readonly undo = output<void>();
  readonly record = output<void>();
  readonly save = output<void>();
  readonly revert = output<EditLogEntry>();

  /** History popover open state (local, presentational). */
  protected readonly historyOpen = signal(false);

  private readonly el = inject<ElementRef<HTMLElement>>(ElementRef);

  constructor() {
    // The popover must not survive a slide-out: close it when the bar hides,
    // or it would reappear already-open on the next slide-in.
    effect(() => {
      if (!this.visible()) this.historyOpen.set(false);
    });
  }

  protected toggleHistory(): void {
    this.historyOpen.update((v) => !v);
  }

  /** Escape dismisses the popover (no-op when it's already closed). */
  protected closeHistory(): void {
    this.historyOpen.set(false);
  }

  /** Outside-click dismissal: a click anywhere outside the history cluster
   *  (icon-button + popover) closes the popover. Clicks inside it — including
   *  the toggle button, which owns its own state — are left alone. */
  protected onDocumentClick(ev: Event): void {
    if (!this.historyOpen()) return;
    const hist = this.el.nativeElement.querySelector('.hist');
    if (hist && !hist.contains(ev.target as Node)) this.historyOpen.set(false);
  }
}
