import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from '@angular/core';

/** The Coverage-map / Final-preview segmented control, shared by the build
 *  overview and the tailoring workspace. First segment is `coverage`, second is
 *  `preview`; the host owns the selected value and reacts to {@link change}. */
@Component({
  selector: 'app-view-toggle',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="segmented" role="tablist" aria-label="View mode">
      <button
        type="button"
        role="tab"
        [class.on]="selected() === 'coverage'"
        [attr.aria-selected]="selected() === 'coverage'"
        (click)="change.emit('coverage')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
          <path d="M9 3H5a2 2 0 0 0-2 2v4M15 3h4a2 2 0 0 1 2 2v4M9 21H5a2 2 0 0 1-2-2v-4M15 21h4a2 2 0 0 0 2-2v-4" />
        </svg>
        Coverage map
      </button>
      <button
        type="button"
        role="tab"
        [class.on]="selected() === 'preview'"
        [attr.aria-selected]="selected() === 'preview'"
        (click)="change.emit('preview')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
          <path d="M4 3h11l5 5v13H4z" /><path d="M15 3v5h5M8 13h8M8 17h8" />
        </svg>
        Final preview
      </button>
    </div>
  `,
  styles: `
    :host { display: inline-block; }
    .segmented { display: inline-flex; padding: 3px; gap: 3px; background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px; }
    .segmented button { display: inline-flex; align-items: center; gap: 7px; padding: 7px 15px; border: 0; border-radius: 999px; background: transparent; font: inherit; font-size: 13px; font-weight: 500; color: var(--muted); cursor: pointer; transition: background 0.15s, color 0.15s, box-shadow 0.15s; }
    .segmented button svg { width: 14px; height: 14px; }
    .segmented button.on { background: var(--surface); color: var(--fg); box-shadow: 0 1px 2px color-mix(in oklch, var(--fg) 12%, transparent); }
    .segmented button:not(.on):hover { color: var(--fg); }
    .segmented button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  `,
})
export class ViewToggle {
  readonly selected = input.required<'coverage' | 'preview'>();
  readonly change = output<'coverage' | 'preview'>();
}
