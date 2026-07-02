import { ChangeDetectionStrategy, Component, input } from '@angular/core';

import type { CoverageRow } from './workspace.model';

/** A lighter version of the overview screen's rich gap table: each JD
 *  requirement and the evidence that covers it. Matched (green) / weak (amber) /
 *  unmatched (red) — the state is always labelled, never colour-only. */
@Component({
  selector: 'app-coverage-map',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <p class="intro">
      Each requirement in the posting, and the evidence that covers it. Weak = a
      partial or semantic match; unmatched = no evidence yet.
    </p>
    @if (rows().length === 0) {
      <div class="empty">No gap report on this build.</div>
    } @else {
      <ul class="covmap" role="list">
        @for (row of rows(); track row.name; let i = $index) {
          <li class="cov-row anim-up" [style.animationDelay.ms]="i * 24">
            <span class="cov-dot" [attr.data-t]="row.state" [attr.title]="label(row.state)" aria-hidden="true"></span>
            <div>
              <div class="cov-name">{{ row.name }}</div>
              @if (row.evidence) {
                <div class="cov-ev">{{ row.evidence }}</div>
              } @else {
                <div class="cov-ev faint">No evidence in your profile yet.</div>
              }
            </div>
            <span class="cov-state" [attr.data-t]="row.state">{{ label(row.state) }}</span>
          </li>
        }
      </ul>
    }
  `,
  styles: `
    :host { display: block; margin-top: 18px; }
    .intro { color: var(--muted); font-size: 13.5px; margin: 0 0 6px; max-width: 60ch; }
    .empty { color: var(--faint); font-size: 13px; padding: 22px 4px; }
    .covmap { list-style: none; margin: 0; padding: 0; }
    .cov-row { display: grid; grid-template-columns: 22px 1fr auto; gap: 12px; align-items: start; padding: 13px 4px; border-bottom: 1px solid var(--border); }
    .cov-row:last-child { border-bottom: 0; }
    .cov-dot { margin-top: 5px; width: 11px; height: 11px; border-radius: 3px; }
    .cov-dot[data-t='matched'] { background: var(--success); }
    .cov-dot[data-t='weak'] { background: var(--warn); }
    .cov-dot[data-t='unmatched'] { background: var(--danger); }
    .cov-name { font-family: var(--font-display); font-size: 15px; }
    .cov-ev { font-size: 12.5px; color: var(--muted); margin-top: 2px; }
    .cov-ev.faint { color: var(--faint); }
    .cov-state { font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.06em; text-transform: uppercase; padding: 3px 8px; border-radius: 999px; border: 1px solid var(--border); white-space: nowrap; }
    .cov-state[data-t='matched'] { color: var(--success); }
    .cov-state[data-t='weak'] { color: var(--warn); }
    .cov-state[data-t='unmatched'] { color: var(--danger); }
  `,
  imports: [],
})
export class CoverageMap {
  readonly rows = input.required<CoverageRow[]>();

  protected label(state: CoverageRow['state']): string {
    return state === 'matched' ? 'Matched' : state === 'weak' ? 'Weak' : 'Unmatched';
  }
}
