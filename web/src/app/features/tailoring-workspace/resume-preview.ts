import { ChangeDetectionStrategy, Component, input, output, signal } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';

import type { PreviewLine, PreviewModel } from './workspace.model';

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
            tabindex="0"
            role="textbox"
            aria-label="Skill, editable"
            contenteditable="true"
            (focus)="showPop(s, $event)"
            (mouseenter)="showPop(s, $event)"
            (blur)="onBlur(s, $event)"
            (mouseleave)="hidePop()"
            (keydown.escape)="blurEl($event)"
          >{{ s.text }}</span>
        }
      </div>
    </div>

    <ng-template #lineTpl let-line let-tag="tag">
      <span
        class="prov"
        [attr.data-status]="line.status"
        tabindex="0"
        role="textbox"
        [attr.aria-label]="tag + ', editable. ' + (line.prov?.text || '')"
        contenteditable="true"
        (focus)="showPop(line, $event)"
        (mouseenter)="showPop(line, $event)"
        (blur)="onBlur(line, $event)"
        (mouseleave)="hidePop()"
        (keydown.escape)="blurEl($event)"
      >{{ line.text }}</span>
    </ng-template>

    <!-- provenance popover — positioned to the focused/hovered line -->
    @if (pop(); as p) {
      <div class="pop on" role="status" [style.left.px]="p.x" [style.top.px]="p.y">
        <div class="pl">{{ p.label }}</div>
        {{ p.text }}
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

    /* editable + provenance-marked lines */
    .prov { border-bottom: 1px dotted color-mix(in oklch, var(--accent) 45%, var(--border)); cursor: text; }
    .prov[data-status='unrecorded'] { border-bottom-color: color-mix(in oklch, var(--warn) 60%, var(--border)); }
    .prov[data-status='edited'] { border-bottom-color: color-mix(in oklch, var(--success) 55%, var(--border)); }
    .prov:hover { background: var(--accent-soft); border-radius: 2px; }
    .prov:focus { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 2px; background: var(--accent-soft); }
    .chip-skill {
      display: inline-block; font-family: var(--font-body); font-size: 12.5px; color: var(--fg);
      background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px;
      padding: 3px 10px; margin: 0 5px 6px 0;
    }
    .chip-skill:focus { outline: 2px solid var(--accent); outline-offset: 2px; }
    .chip-skill[data-status='unrecorded'] { border-color: color-mix(in oklch, var(--warn) 50%, var(--border)); }
    .chip-skill[data-status='edited'] { border-color: color-mix(in oklch, var(--success) 50%, var(--border)); }

    .pop {
      position: fixed; z-index: 60; max-width: 280px; background: var(--fg);
      color: oklch(96% 0.01 80); padding: 11px 13px; border-radius: 9px;
      font-family: var(--font-body); font-size: 12.5px; line-height: 1.5;
      box-shadow: 0 12px 30px -12px color-mix(in oklch, var(--fg) 60%, transparent);
    }
    .pop .pl { font-family: var(--font-mono); font-size: 9.5px; letter-spacing: 0.12em; text-transform: uppercase; color: oklch(78% 0.06 60); margin-bottom: 4px; }

    @media (max-width: 720px) {
      .paper { padding: 30px 22px 34px; }
      .p-name { font-size: 24px; }
      .p-role { flex-wrap: wrap; }
    }
  `,
  imports: [NgTemplateOutlet],
})
export class ResumePreview {
  readonly model = input.required<PreviewModel>();
  readonly edit = output<{ key: string; text: string }>();

  protected readonly pop = signal<{ label: string; text: string; x: number; y: number } | null>(null);

  protected showPop(line: PreviewLine, ev: Event): void {
    if (!line.prov) return;
    const el = ev.target as HTMLElement;
    const r = el.getBoundingClientRect();
    this.pop.set({
      label: line.prov.label,
      text: line.prov.text,
      x: Math.min(r.left, window.innerWidth - 300),
      y: r.bottom + 8,
    });
  }

  protected hidePop(): void {
    this.pop.set(null);
  }

  protected blurEl(ev: Event): void {
    (ev.target as HTMLElement).blur();
  }

  /** Capture the edited text on blur; only emit when it actually changed. */
  protected onBlur(line: PreviewLine, ev: Event): void {
    this.hidePop();
    const text = ((ev.target as HTMLElement).innerText ?? '').trim();
    if (text && text !== line.text) {
      this.edit.emit({ key: line.key, text });
    }
  }
}
