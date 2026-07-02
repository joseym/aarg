import { Component, inject, input, output } from '@angular/core';
import { RouterLink, RouterLinkActive } from '@angular/router';

import { BuildsStore } from '../../services/builds-store';
import type { BuildSummary } from '../../models';

/** The left rail: the recent-builds list, filtered live by the topbar input.
 *  Each item links to that build's tailoring workspace. At ≤1080px it becomes
 *  an off-canvas drawer, slid in when `open` is set (the scrim lives in the
 *  shell). Above 1080px it can instead be collapsed to a slim rail via the
 *  chevron toggle at its top edge; `collapsed` is ignored ≤1080px. */
@Component({
  selector: 'app-sidebar',
  imports: [RouterLink, RouterLinkActive],
  host: { '[class.open]': 'open()', '[class.collapsed]': 'collapsed()' },
  template: `
    <aside class="sidebar">
      <button
        class="collapse-toggle"
        type="button"
        [attr.aria-expanded]="!collapsed()"
        [attr.aria-label]="collapsed() ? 'Expand build list' : 'Collapse build list'"
        (click)="toggleCollapse.emit()"
      >
        <svg class="chev" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M15 6l-6 6 6 6" />
        </svg>
      </button>

      @if (collapsed()) {
        <div class="rail-label" aria-hidden="true">BUILDS · {{ store.filtered().length }}</div>
      }

      <div class="side-content">
        <div class="side-head">
          <h2>Recent Builds</h2>
          <div class="side-head-r">
            <span class="count">{{ store.filtered().length }}</span>
            <button class="side-close" type="button" aria-label="Hide recent builds" (click)="closeNav.emit()">
              ✕
            </button>
          </div>
        </div>

        @if (store.loading()) {
          <div class="build-empty">Loading builds…</div>
        } @else if (store.error()) {
          <div class="build-empty">
            Couldn't reach <code>aarg serve</code>.<br />Start it with
            <code>aarg serve</code> on :8787.
          </div>
        } @else if (store.filtered().length === 0) {
          <div class="build-empty">No builds yet. Start one with <b>New Build</b>.</div>
        } @else {
          <div class="build-list">
            @for (b of store.filtered(); track b.id) {
              <a
                class="build-item anim-up"
                [routerLink]="['/build', b.id, 'tailor']"
                routerLinkActive="active"
                (click)="closeNav.emit()"
              >
                <div class="bi-top">
                  <div>
                    <div class="bi-title">{{ b.title }}</div>
                    @if (b.company) {
                      <div class="bi-co">{{ b.company }}</div>
                    }
                  </div>
                  <span class="score-badge" [attr.data-tier]="tier(b)">{{ pct(b.score) }}</span>
                </div>
                <div class="bi-date">{{ b.created_at }}</div>
              </a>
            }
          </div>
        }
      </div>
    </aside>
  `,
  styles: `
    :host { display: contents; }
    .sidebar {
      position: relative;
      border-right: 1px solid var(--border);
      padding: 20px 14px 40px;
      background: color-mix(in oklch, var(--bg) 60%, var(--surface-2));
      overflow: hidden;
    }

    /* Fixed width so the list's text never squishes/reflows while the
     * shell's grid column animates between expanded and collapsed — it
     * simply fades out, clipped by .sidebar's own overflow:hidden. */
    .side-content {
      width: 260px;
      transition: opacity 0.18s ease;
    }

    .collapse-toggle {
      display: none;
      align-items: center; justify-content: center;
      position: absolute; top: 14px; right: 10px; z-index: 1;
      width: 26px; height: 26px;
      border: 1px solid var(--border); border-radius: 7px;
      background: var(--surface); color: var(--muted); cursor: pointer;
      transition: right 0.25s cubic-bezier(0.2, 0.7, 0.2, 1), transform 0.15s,
        border-color 0.15s, color 0.15s;
    }
    .collapse-toggle:hover { border-color: var(--fg); color: var(--fg); }
    .collapse-toggle:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .collapse-toggle .chev { width: 14px; height: 14px; transition: transform 0.2s; }

    .rail-label {
      position: absolute; top: 58px; left: 50%; z-index: 1;
      transform: translateX(-50%) rotate(180deg);
      writing-mode: vertical-rl;
      font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.12em;
      text-transform: uppercase; color: var(--faint); white-space: nowrap;
    }

    /* Desktop-only collapse behavior. ≤1080px this rule block doesn't apply
     * at all, so the drawer's own transform-based open/close is untouched. */
    @media (min-width: 1081px) {
      .collapse-toggle { display: inline-flex; }
      :host(.collapsed) .collapse-toggle { right: 50%; transform: translateX(50%); }
      :host(.collapsed) .collapse-toggle .chev { transform: rotate(180deg); }
      :host(.collapsed) .side-content {
        opacity: 0;
        visibility: hidden;
        pointer-events: none;
      }
    }

    @media (prefers-reduced-motion: reduce) {
      .side-content,
      .collapse-toggle,
      .collapse-toggle .chev {
        transition: none !important;
      }
    }

    .side-head {
      display: flex; align-items: baseline; justify-content: space-between;
      padding: 0 8px 12px;
    }
    .side-head h2 { font-size: 14px; letter-spacing: 0.02em; }
    .side-head-r { display: flex; align-items: center; gap: 10px; }
    .count { font-family: var(--font-mono); font-size: 11px; color: var(--faint); }
    .side-close {
      display: none; align-items: center; justify-content: center;
      width: 30px; height: 30px; border: 1px solid var(--border); border-radius: 8px;
      background: var(--surface); color: var(--muted); cursor: pointer; font-size: 14px;
    }
    .side-close:hover { border-color: var(--fg); color: var(--fg); }

    .build-list { display: flex; flex-direction: column; gap: 4px; }
    .build-item {
      display: block; width: 100%; text-align: left; text-decoration: none;
      color: inherit; padding: 12px 12px 12px 14px;
      border: 1px solid transparent; border-radius: var(--radius-lg);
      background: transparent; position: relative;
      transition: background 0.14s, border-color 0.14s, box-shadow 0.2s,
        transform 0.14s cubic-bezier(0.2, 0.7, 0.2, 1);
    }
    .build-item:hover { background: var(--surface); transform: translateY(-1px); }
    .build-item.active {
      background: var(--surface); border-color: var(--border);
      box-shadow: inset 3px 0 0 var(--accent);
    }
    .bi-top { display: flex; align-items: flex-start; justify-content: space-between; gap: 10px; }
    .bi-title { font-family: var(--font-display); font-size: 16px; line-height: 1.25; }
    .bi-co { font-size: 13px; color: var(--muted); margin-top: 1px; }
    .bi-date {
      font-family: var(--font-mono); font-size: 11px; color: var(--faint);
      margin-top: 8px; letter-spacing: 0.02em;
    }

    .score-badge {
      flex-shrink: 0;
      font-family: var(--font-mono); font-variant-numeric: tabular-nums;
      font-size: 12.5px; font-weight: 600;
      padding: 3px 8px; border-radius: 6px;
      border: 1px solid var(--border); color: var(--fg); background: var(--bg);
      transition: transform 0.16s cubic-bezier(0.2, 0.7, 0.2, 1);
    }
    .build-item:hover .score-badge { transform: scale(1.05); }
    .score-badge[data-tier='high'] {
      color: var(--success); border-color: color-mix(in oklch, var(--success) 40%, var(--border));
    }
    .score-badge[data-tier='mid'] {
      color: var(--warn); border-color: color-mix(in oklch, var(--warn) 40%, var(--border));
    }
    .score-badge[data-tier='low'] {
      color: var(--danger); border-color: color-mix(in oklch, var(--danger) 40%, var(--border));
    }

    .build-empty {
      padding: 22px 12px; color: var(--faint); font-size: 12.5px;
      line-height: 1.5; text-align: center;
    }
    .build-empty code { font-family: var(--font-mono); color: var(--muted); }

    /* ≤1080px: the rail slides in from the left as a drawer. */
    @media (max-width: 1080px) {
      .sidebar {
        position: fixed; top: 0; left: 0; height: 100dvh;
        width: 300px; max-width: 86vw; z-index: 40;
        transform: translateX(-100%);
        transition: transform 0.28s cubic-bezier(0.2, 0.7, 0.2, 1);
        overflow-y: auto; border-right: 1px solid var(--border);
        box-shadow: 18px 0 50px -30px color-mix(in oklch, var(--fg) 60%, transparent);
      }
      :host(.open) .sidebar { transform: none; }
      .side-close { display: inline-flex; }
      /* Collapse is a desktop-only affordance; ≤1080px the drawer always
       * opens full-width regardless of the persisted desktop choice. */
      .collapse-toggle,
      .rail-label {
        display: none !important;
      }
    }
  `,
})
export class Sidebar {
  protected readonly store = inject(BuildsStore);
  readonly open = input(false);
  /** Desktop-only collapse state (meaningless ≤1080px; see host CSS). */
  readonly collapsed = input(false);
  readonly closeNav = output<void>();
  readonly toggleCollapse = output<void>();

  /** Scores arrive as a 0..1 fraction; show a whole percentage. */
  protected pct(score: number): number {
    return Math.round(score * 100);
  }

  protected tier(b: BuildSummary): 'high' | 'mid' | 'low' {
    if (b.score >= 0.8) return 'high';
    if (b.score >= 0.6) return 'mid';
    return 'low';
  }
}
