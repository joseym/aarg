import { ChangeDetectionStrategy, Component, ElementRef, effect, inject, input, output, signal } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';

import type { LineStatus, PreviewLine, PreviewModel } from './workspace.model';

/** The left pane: the tailored résumé rendered as the export's "paper". Every
 *  line is free-editable (contenteditable) and provenance-aware — hovering or
 *  focusing a line shows where it traces to. Edits are captured on blur and
 *  emitted; the container re-runs the deterministic provenance check and feeds
 *  a fresh model back in (edited lines then read "your own edit").
 *
 *  All user/model text reaches the DOM through interpolation ({{ }}), never
 *  innerHTML — the never-fabricate discipline extends to XSS safety. */
@Component({
  selector: 'app-resume-preview',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @let m = model();
    <div class="paper" role="document" aria-label="Résumé preview">
      <div class="p-name">{{ m.name }}</div>
      <div class="p-target">{{ m.targetTitle }}</div>
      <div class="p-contact">{{ m.contact }}</div>
      <hr />

      <h4>Summary</h4>
      <p class="p-lede">
        <ng-container [ngTemplateOutlet]="lineTpl" [ngTemplateOutletContext]="{ $implicit: m.summary, tag: 'summary line' }" />
      </p>

      <h4>Experience</h4>
      @for (role of m.roles; track role.id) {
        <div class="p-role">
          <b>{{ role.title }}</b><span class="when">{{ role.dates }}</span>
        </div>
        <div class="p-co">{{ role.company }}</div>
        <ul>
          @for (b of role.bullets; track b.key) {
            <li>
              <ng-container
                [ngTemplateOutlet]="lineTpl"
                [ngTemplateOutletContext]="{ $implicit: b, tag: role.company + ' bullet' }"
              />
            </li>
          }
        </ul>
      }

      <h4>Skills</h4>
      <div class="p-skills">
        @for (s of m.skills; track s.key) {
          <span
            class="chip-skill"
            [class.prov]="!!s.status"
            [attr.data-status]="s.status"
            [attr.data-key]="s.key"
            tabindex="0"
            role="textbox"
            aria-label="Skill, editable"
            contenteditable="true"
            (focus)="onFocus(s, $event)"
            (mouseenter)="showPop(s, $event)"
            (blur)="onBlur(s, $event)"
            (mouseleave)="onLineLeave()"
            (keydown.escape)="blurEl($event)"
          >{{ s.text }}</span>
        }
      </div>

      @if (m.projects.length) {
        <h4>Projects</h4>
        @for (proj of m.projects; track proj.id) {
          <div class="p-role">
            <b>{{ proj.name }}</b>
            @if (proj.url) {
              <a class="p-proj-link" [href]="proj.url" target="_blank" rel="noopener">{{ proj.url }}</a>
            }
          </div>
          <p class="p-proj-summary">
            <ng-container
              [ngTemplateOutlet]="lineTpl"
              [ngTemplateOutletContext]="{ $implicit: proj.summary, tag: proj.name + ' summary' }"
            />
          </p>
        }
      }
    </div>

    <ng-template #lineTpl let-line let-tag="tag">
      <span
        class="prov"
        [attr.data-status]="line.status"
        [attr.data-key]="line.key"
        tabindex="0"
        role="textbox"
        [attr.aria-label]="tag + ', editable. ' + (line.prov?.text || '')"
        contenteditable="true"
        (focus)="onFocus(line, $event)"
        (mouseenter)="showPop(line, $event)"
        (blur)="onBlur(line, $event)"
        (mouseleave)="onLineLeave()"
        (keydown.escape)="blurEl($event)"
      >{{ line.text }}</span>
    </ng-template>

    <!-- provenance popover — positioned to the focused/hovered line -->
    @if (pop(); as p) {
      <div class="pop on" role="status" [style.left.px]="p.x" [style.top.px]="p.y">
        <div class="pl">{{ p.label }}</div>
        {{ p.text }}
        @if (p.status === 'unrecorded') {
          <button
            class="pop-confirm"
            type="button"
            (mousedown)="$event.preventDefault()"
            (click)="confirmLine()"
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
      padding: 44px 48px 48px; margin: 18px auto 0; max-width: 860px; font-family: var(--font-display);
      box-shadow: 0 1px 0 var(--border), 0 20px 44px -30px color-mix(in oklch, var(--fg) 42%, transparent);
    }
    .p-name { font-size: 27px; letter-spacing: -0.01em; }
    .p-target { font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.12em; text-transform: uppercase; color: var(--accent); margin-top: 5px; }
    .p-contact { font-family: var(--font-mono); font-size: 11px; color: var(--muted); margin-top: 7px; }
    .paper hr { border: 0; border-top: 1px solid var(--border-ink); margin: 18px 0 20px; }
    .paper h4 { font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.16em; text-transform: uppercase; color: var(--accent); margin: 22px 0 11px; }
    .p-lede { font-family: var(--font-body); font-size: 13.5px; line-height: 1.62; margin: 0; }
    .p-role { display: flex; justify-content: space-between; align-items: baseline; gap: 14px; margin-top: 15px; font-family: var(--font-body); }
    .p-role:first-of-type { margin-top: 0; }
    .p-role b { font-size: 14.5px; font-weight: 600; }
    .p-role .when { font-family: var(--font-mono); font-size: 11px; color: var(--faint); white-space: nowrap; }
    .p-co { font-family: var(--font-body); font-size: 13px; color: var(--muted); margin: 1px 0 8px; }
    .paper ul { margin: 0; padding-left: 17px; font-family: var(--font-body); font-size: 13.5px; line-height: 1.55; }
    .paper li { margin-bottom: 6px; }
    .p-skills { margin-top: 2px; }
    .p-proj-link { font-family: var(--font-mono); font-size: 11px; color: var(--accent); text-decoration: none; white-space: nowrap; }
    .p-proj-link:hover { text-decoration: underline; }
    .p-proj-summary { font-family: var(--font-body); font-size: 13px; color: var(--muted); margin: 2px 0 0; line-height: 1.55; }

    /* editable + provenance-marked lines */
    .prov { border-bottom: 1px dotted color-mix(in oklch, var(--accent) 45%, var(--border)); cursor: text; }
    /* Unrecorded lines are THE lines to track — give them a highlighter wash,
       not just a tinted underline, so they read at a glance. Background-only
       (no pseudo-content) keeps the contenteditable caret and text reads safe;
       box-decoration-break keeps the wash continuous across wrapped lines. */
    .prov[data-status='unrecorded'] {
      background: color-mix(in oklch, var(--warn) 14%, transparent);
      border-bottom: 2px solid color-mix(in oklch, var(--warn) 80%, var(--border));
      border-radius: 3px;
      padding: 1px 3px;
      -webkit-box-decoration-break: clone;
      box-decoration-break: clone;
    }
    .prov[data-status='edited'] { border-bottom-color: color-mix(in oklch, var(--success) 55%, var(--border)); }
    .prov:hover { background: var(--accent-soft); border-radius: 2px; }
    .prov[data-status='unrecorded']:hover { background: color-mix(in oklch, var(--warn) 22%, transparent); }
    .prov:focus { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 2px; background: var(--accent-soft); }
    .chip-skill {
      display: inline-block; font-family: var(--font-body); font-size: 12.5px; color: var(--fg);
      background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px;
      padding: 3px 10px; margin: 0 5px 6px 0;
    }
    .chip-skill:focus { outline: 2px solid var(--accent); outline-offset: 2px; }
    .chip-skill[data-status='unrecorded'] {
      border-color: color-mix(in oklch, var(--warn) 70%, var(--border));
      background: color-mix(in oklch, var(--warn) 14%, var(--surface-2));
    }
    .chip-skill[data-status='edited'] { border-color: color-mix(in oklch, var(--success) 50%, var(--border)); }

    .pop {
      /* z 75: above the pending-edits bar (z 70) — this popover is the user's
         ACTIVE focus (it carries the confirm-as-evidence button), so it outranks
         the passive bar; a line near the viewport bottom must not put the
         confirm button behind the bar's own click targets. */
      position: fixed; z-index: 75; max-width: 280px; background: var(--fg);
      color: oklch(96% 0.01 80); padding: 11px 13px; border-radius: 9px;
      font-family: var(--font-body); font-size: 12.5px; line-height: 1.5;
      box-shadow: 0 12px 30px -12px color-mix(in oklch, var(--fg) 60%, transparent);
    }
    .pop .pl { font-family: var(--font-mono); font-size: 9.5px; letter-spacing: 0.12em; text-transform: uppercase; color: oklch(78% 0.06 60); margin-bottom: 4px; }
    .pop-confirm {
      display: block; margin-top: 9px; width: 100%; padding: 7px 10px; border-radius: 7px;
      border: 1px solid transparent; background: var(--accent); color: oklch(97% 0.02 40);
      font: inherit; font-size: 12px; font-weight: 600; cursor: pointer; text-align: left;
    }
    .pop-confirm:hover { background: var(--accent-2); }
    .pop-confirm:focus-visible { outline: 2px solid oklch(96% 0.01 80); outline-offset: 2px; }

    /* Badge → line: a temporary warn-tinted pulse on the jumped-to line. */
    @keyframes cc-pulse {
      0% { background: color-mix(in oklch, var(--warn) 36%, transparent); box-shadow: 0 0 0 5px color-mix(in oklch, var(--warn) 22%, transparent); }
      100% { background: transparent; box-shadow: 0 0 0 0 transparent; }
    }
    .pulse { animation: cc-pulse 1.6s ease-out; border-radius: 3px; }
    @media (prefers-reduced-motion: reduce) {
      .pulse { animation: none; outline: 2px solid var(--warn); outline-offset: 2px; border-radius: 3px; }
    }

    @media (max-width: 720px) {
      .paper { padding: 30px 22px 34px; }
      .p-name { font-size: 24px; }
      .p-role { flex-wrap: wrap; }
    }
  `,
  imports: [NgTemplateOutlet],
})
export class ResumePreview {
  private readonly host: ElementRef<HTMLElement> = inject(ElementRef);

  readonly model = input.required<PreviewModel>();
  /** The line key the container wants spotlighted (badge → jump). A change
   *  scrolls that line into view and pulses it; null clears. */
  readonly highlightKey = input<string | null>(null);
  readonly edit = output<{ key: string; text: string }>();
  /** The user confirmed an unrecorded line as their own evidence, carrying the
   *  line's CURRENT text (which may be the payload's original, un-edited prose). */
  readonly confirm = output<{ key: string; text: string }>();

  protected readonly pop = signal<{
    key: string;
    status: LineStatus | null;
    label: string;
    text: string;
    x: number;
    y: number;
  } | null>(null);

  /** Whether a line currently holds focus. Keeps the popover open on
   *  `mouseleave` (so its confirm button stays reachable) — a hover-only popover
   *  would vanish before the pointer crossed the gap to the button. */
  private readonly focused = signal(false);
  /** The element the popover is anchored to — read for the line's live text when
   *  confirming (it may hold an un-blurred edit). */
  private popEl: HTMLElement | null = null;

  constructor() {
    // Badge → line: scroll the targeted line into view and pulse it. Restarts
    // the animation on each distinct key so cycling re-highlights.
    effect(() => {
      const key = this.highlightKey();
      if (!key) return;
      const el = this.host.nativeElement.querySelector<HTMLElement>(`[data-key="${cssAttr(key)}"]`);
      if (!el) return;
      el.scrollIntoView({ block: 'center', behavior: 'smooth' });
      el.classList.remove('pulse');
      void el.offsetWidth; // force reflow so the animation replays
      el.classList.add('pulse');
      setTimeout(() => el.classList.remove('pulse'), 1700);
    });
  }

  protected showPop(line: PreviewLine, ev: Event): void {
    if (!line.prov) return;
    const el = ev.target as HTMLElement;
    this.popEl = el;
    const r = el.getBoundingClientRect();
    this.pop.set({
      key: line.key,
      status: line.status,
      label: line.prov.label,
      text: line.prov.text,
      x: Math.min(r.left, window.innerWidth - 300),
      y: r.bottom + 8,
    });
  }

  protected onFocus(line: PreviewLine, ev: Event): void {
    this.focused.set(true);
    this.showPop(line, ev);
  }

  protected hidePop(): void {
    this.pop.set(null);
    this.popEl = null;
  }

  /** Pointer left a line: keep the popover if the line is still focused (so its
   *  confirm button stays reachable); otherwise this was a hover, so dismiss. */
  protected onLineLeave(): void {
    if (!this.focused()) this.hidePop();
  }

  protected blurEl(ev: Event): void {
    (ev.target as HTMLElement).blur();
  }

  /** Capture the edited text on blur; only emit when it actually changed. */
  protected onBlur(line: PreviewLine, ev: Event): void {
    this.focused.set(false);
    this.hidePop();
    const text = ((ev.target as HTMLElement).innerText ?? '').trim();
    if (text && text !== line.text) {
      this.edit.emit({ key: line.key, text });
    }
  }

  /** Confirm the currently-popover'd unrecorded line as evidence. The button's
   *  `mousedown` preventDefault keeps the line focused (no blur), so its live
   *  text is read here and emitted; the container records it to the dataset. */
  protected confirmLine(): void {
    const p = this.pop();
    if (!p || p.status !== 'unrecorded') return;
    const text = ((this.popEl?.innerText ?? '') || '').trim();
    if (text) this.confirm.emit({ key: p.key, text });
    this.hidePop();
  }
}

/** Escape a line key for use inside a `[data-key="…"]` attribute selector. Keys
 *  hold `:` and `-`, so a quoted value is enough once quotes/backslashes are
 *  escaped — avoids depending on `CSS.escape` for the plain characters we emit. */
function cssAttr(key: string): string {
  return key.replace(/["\\]/g, '\\$&');
}
