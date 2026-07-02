import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  effect,
  input,
  output,
  viewChild,
} from '@angular/core';

import type { CopilotKind, ObjectionVM } from './workspace.model';

const COPILOT_COPY: Record<CopilotKind, { kicker: string; title: string; blurb: string }> = {
  strengthen: {
    kicker: 'EVIDENCE INTERVIEW · STRENGTHEN A LINE',
    title: 'A stronger line — in your own scope',
    blurb: 'The strengthen copilot will interview you for what actually happened, then rephrase the flagged line without widening its scope.',
  },
  metric: {
    kicker: 'EVIDENCE INTERVIEW · METRIC CAPTURE',
    title: 'What was the measurable result?',
    blurb: 'The metric copilot will ask for a number and fold your exact words into the bullet — it never proposes a figure.',
  },
  summary: {
    kicker: 'EVIDENCE INTERVIEW · SUMMARY',
    title: 'Edit your summary in your own words',
    blurb: 'The summary copilot will open your existing summary for a free edit; whatever you save becomes authoritative.',
  },
  skills: {
    kicker: 'EVIDENCE INTERVIEW · SKILL CHECK',
    title: 'The job asks for these — which do you have?',
    blurb: 'The skills copilot will run the JD skill check; anything you leave unchecked is recorded as “don’t have” and won’t be re-asked.',
  },
  layout: {
    kicker: 'LAYOUT ADJUSTMENT',
    title: 'A presentation-only change',
    blurb: 'The layout copilot routes to the variant adapter — presentation only, the canonical claims never change.',
  },
};

/** A right-side drawer that, for this wave, names which copilot a Refine action
 *  would open. The interactive wasm exports (`strengthen_interactive`,
 *  `capture_metrics_interactive`, …) need user-callback modals that land in the
 *  next wave — this is the seam where they hook in.
 *
 *  Adds the a11y the raw export lacked: role="dialog", a focus trap, Escape to
 *  close, and focus restoration. */
@Component({
  selector: 'app-refine-drawer',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (objection(); as o) {
      @let copy = copyFor(o.copilot);
      <div class="scrim on" (click)="close.emit()"></div>
      <div
        #panel
        class="drawer on"
        role="dialog"
        aria-modal="true"
        [attr.aria-label]="copy.title"
        (keydown)="onKeydown($event)"
      >
        <div class="cp-head">
          <div>
            <div class="label k">{{ copy.kicker }}</div>
            <h3>{{ copy.title }}</h3>
          </div>
          <button #closeBtn class="x" type="button" (click)="close.emit()" aria-label="Close">✕</button>
        </div>
        <div class="cp-body">
          <div class="cp-context">
            <span class="label pl">Objection</span>{{ o.message }}
          </div>
          @if (o.flaggedText) {
            <div class="cp-line">{{ o.flaggedText }}</div>
          }
          <div class="assist">
            <span class="dot" aria-hidden="true"></span>
            <div>{{ copy.blurb }}</div>
          </div>
          <div class="stub-note">
            <b>Copilot arrives next wave.</b> This will route to the
            <code>{{ o.copilot }}</code> copilot, driving the interactive core
            export through a user-callback modal. Nothing is changed yet.
          </div>
        </div>
        <div class="cp-foot">
          <button class="btn btn-ghost" type="button" (click)="close.emit()">Close</button>
          <button class="btn" type="button" disabled>Open {{ o.copilot }} copilot →</button>
        </div>
      </div>
    }
  `,
  styles: `
    :host { display: contents; }
    .scrim { position: fixed; inset: 0; z-index: 40; background: color-mix(in oklch, var(--fg) 32%, transparent); backdrop-filter: blur(2px); }
    .drawer {
      position: fixed; top: 0; right: 0; height: 100vh; width: 520px; max-width: 94vw; z-index: 50;
      background: var(--bg); border-left: 1px solid var(--border);
      box-shadow: -24px 0 60px -34px color-mix(in oklch, var(--fg) 55%, transparent);
      display: flex; flex-direction: column;
    }
    .cp-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; padding: 22px 24px 16px; border-bottom: 1px solid var(--border); }
    .cp-head h3 { font-size: 20px; margin-top: 8px; letter-spacing: -0.01em; }
    .label { font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.14em; text-transform: uppercase; color: var(--faint); }
    .label.k { color: var(--accent); }
    .x { width: 32px; height: 32px; border-radius: 8px; border: 1px solid var(--border); background: var(--surface); display: grid; place-items: center; cursor: pointer; color: inherit; }
    .x:hover { border-color: var(--fg); }
    .cp-body { padding: 22px 24px; overflow-y: auto; flex: 1; }
    .cp-foot { padding: 16px 24px; border-top: 1px solid var(--border); display: flex; align-items: center; justify-content: space-between; gap: 12px; }
    .cp-context { padding: 12px 14px; background: var(--surface-2); border: 1px solid var(--border); border-radius: 9px; font-size: 14px; line-height: 1.45; margin-bottom: 14px; }
    .cp-context .pl { display: block; margin-bottom: 5px; }
    .cp-line { font-family: var(--font-display); font-size: 14px; line-height: 1.4; padding: 10px 12px; border-left: 2px solid var(--border); background: var(--surface-2); border-radius: 0 6px 6px 0; margin-bottom: 16px; }
    .assist { display: flex; gap: 9px; align-items: flex-start; padding: 11px 13px; border-radius: 9px; background: var(--accent-soft); font-size: 13px; color: var(--fg); line-height: 1.5; margin-bottom: 16px; }
    .assist .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); margin-top: 5px; flex-shrink: 0; }
    .stub-note { font-size: 12.5px; color: var(--muted); line-height: 1.55; border: 1px dashed var(--border); border-radius: 9px; padding: 12px 14px; }
    .stub-note code { font-family: var(--font-mono); color: var(--accent); }
    .btn { display: inline-flex; align-items: center; height: 34px; padding: 0 14px; border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface); font: inherit; font-size: 14px; color: inherit; cursor: pointer; }
    .btn:hover:not(:disabled) { border-color: var(--fg); }
    .btn:disabled { opacity: 0.55; cursor: default; }
    .btn-ghost { border-color: transparent; background: transparent; }
    .btn:focus-visible, .x:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  `,
  imports: [],
})
export class RefineDrawer {
  readonly objection = input<ObjectionVM | null>(null);
  readonly close = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLElement>>('panel');
  private readonly closeBtn = viewChild<ElementRef<HTMLButtonElement>>('closeBtn');
  private opener: HTMLElement | null = null;

  constructor() {
    effect(() => {
      if (this.objection()) {
        // Remember the trigger so focus can return to it on close.
        this.opener = document.activeElement as HTMLElement | null;
        queueMicrotask(() => this.closeBtn()?.nativeElement.focus());
      } else if (this.opener) {
        this.opener.focus();
        this.opener = null;
      }
    });
  }

  protected copyFor(kind: CopilotKind) {
    return COPILOT_COPY[kind];
  }

  protected onKeydown(ev: KeyboardEvent): void {
    if (ev.key === 'Escape') {
      ev.preventDefault();
      this.close.emit();
      return;
    }
    if (ev.key !== 'Tab') return;
    // Focus trap: cycle within the drawer's focusable elements.
    const root = this.panel()?.nativeElement;
    if (!root) return;
    const items = Array.from(
      root.querySelectorAll<HTMLElement>('button:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])'),
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
