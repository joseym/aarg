import { ChangeDetectionStrategy, Component, input, output, signal } from '@angular/core';

import type { CoverProvenanceReport, ParagraphProvenance } from '../../models';
import { coverStatusExplainer, coverStatusLabel } from './cover-preview.model';

/** The Cover Letter view's editing pane: the drafted letter rendered as prose,
 *  one contenteditable paragraph per body paragraph, each carrying its
 *  provenance status. This is the paragraph analog of {@link ResumePreview} —
 *  deliberately NOT the same component, because a cover letter is a flat list of
 *  paragraphs, not the role/bullet/skill structure a résumé preview is built
 *  around, and its three states (Grounded / Unrecorded / Exempt) differ from the
 *  résumé's line states.
 *
 *  The greeting and sign-off are code-filled (drawn from the posting and the
 *  résumé's contact block), never model-authored, so they are shown static and
 *  never classified — mirroring the domain classifier, which never sees them.
 *
 *  Edits are local and in-memory: a paragraph's blur emits {@link edit} upward,
 *  the container re-runs the deterministic classifier and feeds a fresh report
 *  back in. Nothing here persists — Regenerate remains the durable path.
 *
 *  All text reaches the DOM through interpolation ({{ }}), never innerHTML, so
 *  the never-fabricate discipline extends to XSS safety. */
@Component({
  selector: 'app-cover-preview',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @let r = report();
    <div class="paper" role="document" aria-label="Cover letter preview" [class.checking]="checking()">
      @if (checking()) {
        <p class="cl-checking" role="status"><span class="spin" aria-hidden="true"></span> Rechecking paragraphs…</p>
      }
      @if (greeting()) {
        <p class="cl-greeting">{{ greeting() }}</p>
      }

      @for (p of r.paragraphs; track $index) {
        <p
          class="para"
          [attr.data-status]="p.status"
          [attr.data-index]="$index"
          tabindex="0"
          role="textbox"
          [attr.aria-label]="'Cover letter paragraph, editable. ' + statusLabel(p)"
          contenteditable="true"
          (focus)="onFocus($index, p, $event)"
          (mouseenter)="showPop($index, p, $event)"
          (blur)="onBlur($index, p, $event)"
          (mouseleave)="onLeave()"
          (keydown.escape)="blurEl($event)"
        >{{ p.text }}</p>
      }

      @if (signoff()) {
        <p class="cl-signoff">{{ signoff() }}</p>
      }
    </div>

    <p class="cl-note">
      These edits stay in your browser and aren't saved. Use Regenerate to write a fresh letter to this build.
    </p>

    <!-- provenance popover — anchored to the focused or hovered paragraph -->
    @if (pop(); as p) {
      <div class="pop on" role="status" [style.left.px]="p.x" [style.top.px]="p.y">
        <div class="pl" [attr.data-status]="p.status">{{ p.label }}</div>
        {{ p.text }}
        @if (p.status === 'unrecorded') {
          <button
            class="pop-confirm"
            type="button"
            (mousedown)="$event.preventDefault()"
            (click)="confirmParagraph()"
          >
            This is true: record it as evidence
          </button>
        }
      </div>
    }
  `,
  styles: `
    :host { display: block; position: relative; }
    .paper {
      background: oklch(99% 0.004 96); border: 1px solid var(--border); border-radius: 6px;
      padding: 40px 48px 44px; margin: 18px auto 0; max-width: 760px;
      font-family: var(--font-body); font-size: 14px; line-height: 1.62; color: var(--fg);
      box-shadow: 0 1px 0 var(--border), 0 20px 44px -30px color-mix(in oklch, var(--fg) 42%, transparent);
      position: relative;
    }
    /* While a recheck is in flight the paragraph statuses may be stale, so dim
       the sheet slightly — the change is visible in-progress, never frozen. */
    .paper.checking .para { opacity: 0.72; transition: opacity 0.15s; }
    .cl-checking {
      display: inline-flex; align-items: center; gap: 7px; position: absolute; top: 12px; right: 14px;
      margin: 0; font-family: var(--font-mono); font-size: 11px; color: var(--muted);
      background: var(--surface); border: 1px solid var(--border); border-radius: 999px; padding: 4px 10px;
    }
    .spin { width: 11px; height: 11px; border-radius: 50%; border: 2px solid var(--border); border-top-color: var(--accent); animation: cp-spin 0.7s linear infinite; }
    @media (prefers-reduced-motion: reduce) { .spin { animation: none; } }
    @keyframes cp-spin { to { transform: rotate(360deg); } }
    .cl-greeting { margin: 0 0 16px; }
    .cl-signoff { margin: 20px 0 0; white-space: pre-line; }

    /* Each body paragraph is editable and status-marked. A left rail carries the
       status at a glance; unrecorded also gets a faint wash so it reads as the
       one that needs attention. Exempt stays deliberately quiet — connective
       language is neither flagged nor specially blessed. */
    .para {
      margin: 0 0 14px; padding: 4px 10px; border-left: 3px solid transparent;
      border-radius: 3px; cursor: text; transition: background 0.12s;
    }
    .para:last-of-type { margin-bottom: 0; }
    .para[data-status='grounded'] { border-left-color: color-mix(in oklch, var(--success) 60%, var(--border)); }
    .para[data-status='exempt'] { border-left-color: var(--border-ink); }
    .para[data-status='unrecorded'] {
      border-left-color: color-mix(in oklch, var(--warn) 80%, var(--border));
      background: color-mix(in oklch, var(--warn) 10%, transparent);
    }
    .para:hover { background: var(--accent-soft); }
    .para[data-status='unrecorded']:hover { background: color-mix(in oklch, var(--warn) 18%, transparent); }
    .para:focus { outline: 2px solid var(--accent); outline-offset: 2px; background: var(--accent-soft); }
    .para[data-status='unrecorded']:focus { background: color-mix(in oklch, var(--warn) 14%, transparent); }

    .cl-note { margin: 12px auto 0; max-width: 760px; font-size: 12px; color: var(--faint); font-style: italic; }

    .pop {
      position: fixed; z-index: 75; max-width: 300px; background: var(--fg);
      color: oklch(96% 0.01 80); padding: 11px 13px; border-radius: 9px;
      font-family: var(--font-body); font-size: 12.5px; line-height: 1.5;
      box-shadow: 0 12px 30px -12px color-mix(in oklch, var(--fg) 60%, transparent);
    }
    .pop .pl { font-family: var(--font-mono); font-size: 9.5px; letter-spacing: 0.12em; text-transform: uppercase; margin-bottom: 4px; color: oklch(78% 0.06 60); }
    .pop .pl[data-status='grounded'] { color: oklch(82% 0.13 150); }
    .pop .pl[data-status='unrecorded'] { color: oklch(84% 0.13 70); }
    .pop .pl[data-status='exempt'] { color: oklch(80% 0.02 80); }
    .pop-confirm {
      display: block; margin-top: 9px; width: 100%; padding: 7px 10px; border-radius: 7px;
      border: 1px solid transparent; background: var(--accent); color: oklch(97% 0.02 40);
      font: inherit; font-size: 12px; font-weight: 600; cursor: pointer; text-align: left;
    }
    .pop-confirm:hover { background: var(--accent-2); }
    .pop-confirm:focus-visible { outline: 2px solid oklch(96% 0.01 80); outline-offset: 2px; }

    @media (max-width: 720px) {
      .paper { padding: 28px 22px 32px; }
      .cl-note { padding: 0 6px; }
    }
  `,
})
export class CoverPreview {
  /** The classifier's current verdict for the letter's body paragraphs. The
   *  container re-runs the check on each edit and feeds a fresh report in, so
   *  `report().paragraphs[i].text` is always the live (possibly edited) text. */
  readonly report = input.required<CoverProvenanceReport>();
  /** The code-filled greeting line, shown static and never classified. */
  readonly greeting = input<string>('');
  /** The code-filled sign-off block, shown static and never classified. */
  readonly signoff = input<string>('');
  /** Whether a provenance recheck is in flight. The claim half is a real model
   *  call now, so an edit's new status isn't instant; this shows a pending cue
   *  and dims the sheet so the pane reads as working, not frozen or stale. */
  readonly checking = input<boolean>(false);
  /** A paragraph was edited (captured on blur): its index and the new text. The
   *  container splices it into the working copy and re-runs the classifier. */
  readonly edit = output<{ index: number; text: string }>();
  /** The candidate confirmed an unrecorded paragraph as their own evidence:
   *  its index and CURRENT text (which may include an un-blurred edit, read
   *  live off the element — never a stale copy of the report's text). The
   *  container persists it to the build's `CoverBrief.emphasis` and re-checks
   *  locally. Nothing here calls a model: the emitted text is exactly what was
   *  already on the page. */
  readonly confirm = output<{ index: number; text: string }>();

  protected readonly pop = signal<{
    index: number;
    status: string;
    label: string;
    text: string;
    x: number;
    y: number;
  } | null>(null);

  /** Whether a paragraph currently holds focus — keeps the popover open on
   *  `mouseleave` so a focused paragraph's explanation (and, for an unrecorded
   *  one, its confirm button) stays reachable. */
  private focused = false;
  /** The element the popover is anchored to — read for the paragraph's live
   *  text when confirming (it may hold an un-blurred edit). */
  private popEl: HTMLElement | null = null;

  protected statusLabel(p: ParagraphProvenance): string {
    return coverStatusLabel(p.status);
  }

  protected showPop(index: number, p: ParagraphProvenance, ev: Event): void {
    const el = ev.target as HTMLElement;
    this.popEl = el;
    const r = el.getBoundingClientRect();
    this.pop.set({
      index,
      status: p.status,
      label: coverStatusLabel(p.status),
      text: coverStatusExplainer(p),
      x: Math.min(r.left, window.innerWidth - 320),
      y: r.bottom + 8,
    });
  }

  protected onFocus(index: number, p: ParagraphProvenance, ev: Event): void {
    this.focused = true;
    this.showPop(index, p, ev);
  }

  protected onLeave(): void {
    if (!this.focused) this.hidePop();
  }

  protected hidePop(): void {
    this.pop.set(null);
    this.popEl = null;
  }

  protected blurEl(ev: Event): void {
    (ev.target as HTMLElement).blur();
  }

  /** Capture the edited text on blur; only emit when it actually changed. */
  protected onBlur(index: number, p: ParagraphProvenance, ev: Event): void {
    this.focused = false;
    this.hidePop();
    const text = ((ev.target as HTMLElement).innerText ?? '').trim();
    if (text && text !== p.text) {
      this.edit.emit({ index, text });
    }
  }

  /** Confirm the currently popover'd unrecorded paragraph as evidence. The
   *  button's `mousedown` preventDefault keeps the paragraph focused (no blur
   *  first), mirroring {@link ResumePreview}'s confirm action, so the live
   *  (possibly just-edited) text is what's read and emitted here — never a
   *  fresh model call, just the paragraph's own already-shown words. */
  protected confirmParagraph(): void {
    const p = this.pop();
    if (!p || p.status !== 'unrecorded') return;
    const text = ((this.popEl?.innerText ?? '') || '').trim();
    if (text) this.confirm.emit({ index: p.index, text });
    this.hidePop();
  }
}
