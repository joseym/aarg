import { Component, ElementRef, computed, effect, inject, input, output, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { NavigationEnd, Router, RouterLink, RouterLinkActive } from '@angular/router';
import { filter, firstValueFrom, map } from 'rxjs';

import { ApiService } from '../../services/api.service';
import { BuildsStore } from '../../services/builds-store';
import { CopilotHost } from '../../shared/copilot-host';
import type { BuildSummary } from '../../models';

type SortBy = 'newest' | 'score';
type SortDir = 'asc' | 'desc';

/** One rendered section of the list: a run of builds under an optional company
 *  header. `company` is null in the flat (ungrouped) view — a single headerless
 *  section — and the company name (or a fallback) when grouping is on. */
interface BuildSection {
  company: string | null;
  builds: BuildSummary[];
}

const GROUP_KEY = 'aarg.sidebar.group';
const SORT_KEY = 'aarg.sidebar.sort';
const DIR_KEY = 'aarg.sidebar.dir';
const COLLAPSED_KEY = 'aarg.sidebar.folded-groups';
const NO_COMPANY = 'No company';

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
          <div class="list-controls">
            <button
              class="lc-toggle"
              type="button"
              [class.on]="groupByCompany()"
              [attr.aria-pressed]="groupByCompany()"
              (click)="toggleGrouping()"
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                <path d="M3 6h18M6 12h12M9 18h6" />
              </svg>
              <span>Group by company</span>
            </button>
            <label class="lc-sort">
              <span class="lc-sr">Sort builds</span>
              <select [value]="sortBy()" (change)="onSortChange($event)">
                <option value="newest">Newest</option>
                <option value="score">Score</option>
              </select>
            </label>
            @if (groupByCompany()) {
              <button
                class="lc-dir lc-fold"
                type="button"
                [attr.aria-label]="allFolded() ? 'Expand all groups' : 'Collapse all groups'"
                [attr.title]="allFolded() ? 'Expand all' : 'Collapse all'"
                (click)="toggleFoldAll()"
              >
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                  @if (allFolded()) {
                    <path d="M7 9l5 5 5-5" />
                  } @else {
                    <path d="M7 14l5-5 5 5" />
                  }
                </svg>
              </button>
            }
            <button
              class="lc-dir"
              type="button"
              [attr.aria-label]="sortDir() === 'asc' ? 'Sort direction ascending' : 'Sort direction descending'"
              (click)="toggleDir()"
            >
              <svg
                class="dir-arrow"
                [class.up]="sortDir() === 'asc'"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                aria-hidden="true"
              >
                <path d="M12 5v14M6 13l6 6 6-6" />
              </svg>
            </button>
          </div>

          <div class="build-list">
            @for (s of sections(); track s.company ?? '') {
              @if (s.company !== null) {
                <button
                  class="group-head"
                  type="button"
                  [attr.aria-expanded]="!isCollapsed(s.company)"
                  (click)="toggleCollapsed(s.company)"
                >
                  <svg
                    class="gh-chev"
                    [class.closed]="isCollapsed(s.company)"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2.4"
                    aria-hidden="true"
                  >
                    <path d="M8 10l4 4 4-4" />
                  </svg>
                  <span class="gh-name">{{ s.company }}</span>
                  <span class="gh-count">{{ s.builds.length }}</span>
                </button>
              }
              @if (s.company === null || !isCollapsed(s.company)) {
                @for (b of s.builds; track b.id) {
                  <div class="build-row">
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
                    <button
                      class="row-remove"
                      type="button"
                      [attr.aria-label]="'Remove build ' + b.id"
                      (click)="askRemove(b.id, $event)"
                    >
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
                        <path d="M4 7h16M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2M6 7l1 13a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1l1-13" />
                      </svg>
                    </button>
                    @if (confirmingId() === b.id) {
                      <div
                        class="row-confirm"
                        role="alertdialog"
                        aria-label="Confirm build removal"
                        (keydown.escape)="cancelRemove()"
                      >
                        <p>Remove build {{ b.id }}? This permanently deletes its files.</p>
                        <div class="rc-actions">
                          <button type="button" class="rc-go" [disabled]="removing()" (click)="confirmRemove(b.id)">Remove</button>
                          <button type="button" class="rc-cancel" (click)="cancelRemove()">Cancel</button>
                        </div>
                      </div>
                    }
                  </div>
                }
              }
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

    /* Compact grouping + sort row, sitting under the section head. Narrow rail,
     * so house tokens only and no new hues. */
    .list-controls {
      display: flex; align-items: center; gap: 6px; padding: 0 8px 12px;
    }
    .lc-toggle {
      display: inline-flex; align-items: center; gap: 6px; flex: 1; min-width: 0;
      padding: 5px 9px; border: 1px solid var(--border); border-radius: 8px;
      background: var(--surface); color: var(--muted); font-size: 11.5px; cursor: pointer;
      transition: border-color 0.14s, color 0.14s, background 0.14s;
    }
    .lc-toggle:hover { border-color: var(--fg); color: var(--fg); }
    .lc-toggle:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .lc-toggle.on { border-color: var(--accent); color: var(--accent); background: var(--accent-soft); }
    .lc-toggle svg { width: 13px; height: 13px; flex-shrink: 0; }
    .lc-toggle span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .lc-sort { display: flex; }
    .lc-sort .lc-sr {
      position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px;
      overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0;
    }
    .lc-sort select {
      padding: 5px 8px; border: 1px solid var(--border); border-radius: 8px;
      background: var(--surface); color: var(--muted); font-size: 11.5px; cursor: pointer;
      transition: border-color 0.14s, color 0.14s;
    }
    .lc-sort select:hover { border-color: var(--fg); color: var(--fg); }
    .lc-sort select:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .lc-dir {
      display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0;
      width: 28px; height: 28px; padding: 0;
      border: 1px solid var(--border); border-radius: 8px;
      background: var(--surface); color: var(--muted); cursor: pointer;
      transition: border-color 0.14s, color 0.14s;
    }
    .lc-dir:hover { border-color: var(--fg); color: var(--fg); }
    .lc-dir:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .dir-arrow {
      width: 14px; height: 14px;
      transition: transform 0.16s cubic-bezier(0.2, 0.7, 0.2, 1);
    }
    .dir-arrow.up { transform: rotate(180deg); }

    .group-head {
      display: flex; align-items: center; gap: 7px; width: 100%;
      font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.1em;
      text-transform: uppercase; color: var(--faint); text-align: left;
      padding: 12px 10px 5px; background: transparent; border: 0; cursor: pointer;
      transition: color 0.14s;
    }
    .group-head:hover { color: var(--muted); }
    .group-head:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 6px; }
    .group-head:first-child { padding-top: 2px; }
    .gh-chev {
      width: 13px; height: 13px; flex-shrink: 0;
      transition: transform 0.16s cubic-bezier(0.2, 0.7, 0.2, 1);
    }
    .gh-chev.closed { transform: rotate(-90deg); }
    .gh-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .gh-count {
      margin-left: auto; flex-shrink: 0;
      font-variant-numeric: tabular-nums; color: var(--faint);
    }

    .build-list { display: flex; flex-direction: column; gap: 4px; }
    .build-row { position: relative; }
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

    /* Per-row remove affordance: a house-token trash button that stays out of
     * sight until the row is hovered or the button itself is focused, so it
     * never competes with the score badge at rest. */
    .row-remove {
      position: absolute; bottom: 8px; right: 8px;
      display: inline-flex; align-items: center; justify-content: center;
      width: 26px; height: 26px; padding: 0;
      border: 1px solid var(--border); border-radius: 7px;
      background: var(--surface); color: var(--muted); cursor: pointer;
      opacity: 0; pointer-events: none;
      transition: opacity 0.14s, border-color 0.14s, color 0.14s;
    }
    .build-row:hover .row-remove,
    .row-remove:focus-visible {
      opacity: 1; pointer-events: auto;
    }
    .row-remove:hover { border-color: var(--danger); color: var(--danger); }
    /* No hover on touch screens; the control must be reachable there. */
    @media (hover: none) {
      .row-remove { opacity: 1; pointer-events: auto; }
    }
    .row-remove:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .row-remove svg { width: 14px; height: 14px; }

    .row-confirm {
      margin: 4px 2px 2px; padding: 11px 12px;
      border: 1px solid color-mix(in oklch, var(--danger) 40%, var(--border));
      border-radius: var(--radius-lg);
      background: color-mix(in oklch, var(--danger) 8%, var(--surface));
    }
    .row-confirm p { font-size: 12.5px; line-height: 1.4; color: var(--fg); margin: 0 0 9px; }
    .rc-actions { display: flex; gap: 7px; }
    .rc-actions button {
      flex: 1; padding: 6px 10px; border-radius: 7px; font-size: 12px; cursor: pointer;
      border: 1px solid var(--border); background: var(--surface); color: var(--fg);
      transition: border-color 0.14s, background 0.14s, color 0.14s;
    }
    .rc-actions button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
    .rc-go { border-color: var(--danger) !important; color: var(--danger) !important; }
    .rc-go:hover { background: var(--danger) !important; color: var(--bg) !important; }
    .rc-cancel:hover { border-color: var(--fg); }

    @media (prefers-reduced-motion: reduce) {
      .gh-chev, .dir-arrow, .row-remove { transition: none !important; }
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
  private readonly api = inject(ApiService);
  private readonly el = inject<ElementRef<HTMLElement>>(ElementRef);
  private readonly router = inject(Router);
  private readonly copilot = inject(CopilotHost);
  readonly open = input(false);
  /** Desktop-only collapse state (meaningless ≤1080px; see host CSS). */
  readonly collapsed = input(false);
  readonly closeNav = output<void>();
  readonly toggleCollapse = output<void>();

  /** Group builds under company headers. Default ON (the common request is to
   *  cluster by company); the choice persists in localStorage so turning it off
   *  sticks across reloads. */
  protected readonly groupByCompany = signal<boolean>(readGroup());
  /** Sort key, applied inside groups when grouping and across the flat list
   *  otherwise. Persisted alongside the grouping choice. */
  protected readonly sortBy = signal<SortBy>(readSort());
  /** Sort direction. Newest-first (desc) is the default; the choice persists. */
  protected readonly sortDir = signal<SortDir>(readDir());
  /** The set of collapsed groups, by trimmed company name. Persisted so a
   *  folded company stays folded across reloads. */
  private readonly collapsedGroups = signal<Set<string>>(readCollapsed());
  /** Which build row is showing its inline remove confirmation, if any. */
  protected readonly confirmingId = signal<string | null>(null);
  /** True while a DELETE is in flight; blocks a second click from firing a
   *  duplicate that would 404 and toast a false failure over the success. */
  protected readonly removing = signal(false);

  /** The current route's build id, tracked so the open build's group can be
   *  force-expanded (its active row must never sit inside a folded group). */
  private readonly currentUrl = toSignal(
    this.router.events.pipe(
      filter((e) => e instanceof NavigationEnd),
      map(() => this.router.url),
    ),
    { initialValue: this.router.url },
  );
  private readonly activeId = computed(() => {
    const match = this.currentUrl().match(/\/build\/([^/]+)/);
    return match ? decodeURIComponent(match[1]) : null;
  });
  /** The group the open build belongs to (its trimmed company, or "No company"),
   *  or null when no build is open or its summary isn't loaded yet. */
  private readonly activeCompany = computed(() => {
    const id = this.activeId();
    if (!id) return null;
    const build = this.store.builds().find((b) => b.id === id);
    return build ? groupKey(build) : null;
  });

  /** The list to render: one headerless section when grouping is off, else one
   *  section per company (ordered by each group's newest build, empty groups
   *  dropped since they're built from the already-filtered list). The active
   *  sort and direction apply within every section. Composes with the topbar
   *  text filter via `store.filtered()`. */
  protected readonly sections = computed<BuildSection[]>(() => {
    const builds = this.store.filtered();
    const sort = this.sortBy();
    const dir = this.sortDir();
    if (!this.groupByCompany()) {
      return [{ company: null, builds: sortBuilds(builds, sort, dir) }];
    }
    return groupByCompany(builds, sort, dir);
  });

  protected toggleGrouping(): void {
    this.groupByCompany.update((v) => !v);
    writeLs(GROUP_KEY, this.groupByCompany() ? '1' : '0');
  }

  protected onSortChange(e: Event): void {
    const value: SortBy = (e.target as HTMLSelectElement).value === 'score' ? 'score' : 'newest';
    this.sortBy.set(value);
    writeLs(SORT_KEY, value);
  }

  protected toggleDir(): void {
    this.sortDir.update((d) => (d === 'asc' ? 'desc' : 'asc'));
    writeLs(DIR_KEY, this.sortDir());
  }

  /** Whether a group renders folded. Navigating to a build inside a folded
   *  group unfolds that group for real (state and persistence, via the effect
   *  in the constructor), so the header toggle always does what it looks
   *  like it does. */
  protected isCollapsed(company: string): boolean {
    return this.collapsedGroups().has(company);
  }

  constructor() {
    // Opening a build inside a folded group unfolds the group for real, so
    // the persisted state matches what the user sees and the header toggle
    // never turns into a dead control.
    effect(() => {
      const active = this.activeCompany();
      if (active && this.collapsedGroups().has(active)) {
        this.collapsedGroups.update((set) => {
          const next = new Set(set);
          next.delete(active);
          return next;
        });
        writeLs(COLLAPSED_KEY, JSON.stringify([...this.collapsedGroups()]));
      }
    });
  }

  /** Whether everything foldable is folded. The open build's group is exempt
   *  (the unfold-on-navigate effect holds it open), so it does not count
   *  against "all folded" or the button could never flip to expand. */
  protected readonly allFolded = computed(() => {
    const active = this.activeCompany();
    const companies = this.sections()
      .map((s) => s.company)
      .filter((c): c is string => c !== null && c !== active);
    return companies.length > 0 && companies.every((c) => this.collapsedGroups().has(c));
  });

  protected toggleFoldAll(): void {
    const companies = this.sections()
      .map((s) => s.company)
      .filter((c): c is string => c !== null);
    this.collapsedGroups.set(this.allFolded() ? new Set() : new Set(companies));
    writeLs(COLLAPSED_KEY, JSON.stringify([...this.collapsedGroups()]));
  }

  protected toggleCollapsed(company: string): void {
    this.collapsedGroups.update((set) => {
      const next = new Set(set);
      if (next.has(company)) next.delete(company);
      else next.add(company);
      return next;
    });
    writeLs(COLLAPSED_KEY, JSON.stringify([...this.collapsedGroups()]));
  }

  protected askRemove(id: string, e: Event): void {
    // The remove button sits over the row's link; don't let the click navigate.
    e.preventDefault();
    this.lastRemoveTrigger = e.currentTarget as HTMLElement;
    e.stopPropagation();
    this.confirmingId.set(id);
    // Move focus into the dialog so Escape works immediately and screen
    // readers land on the choice; restored on cancel.
    setTimeout(() => {
      const cancel = this.el.nativeElement.querySelector<HTMLElement>('.rc-cancel');
      cancel?.focus();
    });
  }

  protected cancelRemove(): void {
    this.confirmingId.set(null);
    this.lastRemoveTrigger?.focus();
    this.lastRemoveTrigger = null;
  }

  private lastRemoveTrigger: HTMLElement | null = null;

  /** Delete a build: DELETE the server copy, refresh the list, and toast. If the
   *  removed build was the one open, go back to `/`, which redirects to the
   *  newest remaining build or shows first-run. A failure toasts the server's
   *  message and leaves everything as it was. */
  protected async confirmRemove(id: string): Promise<void> {
    if (this.removing()) return;
    this.removing.set(true);
    const wasOpen = this.activeId() === id;
    try {
      await firstValueFrom(this.api.removeBuild(id));
    } catch (err) {
      this.removing.set(false);
      this.confirmingId.set(null);
      this.copilot.notify(removeErrorMessage(err));
      return;
    }
    this.removing.set(false);
    this.confirmingId.set(null);
    this.store.load();
    this.copilot.notify(`Removed build ${id}`);
    if (wasOpen) {
      await this.router.navigate(['/'], { replaceUrl: true });
    }
  }

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

// ── module-local helpers ───────────────────────────────────────────────

/** The group a build belongs to: its trimmed company, or the "No company"
 *  fallback when the company is blank. Also the localStorage key a folded
 *  group is remembered under. */
function groupKey(b: BuildSummary): string {
  return b.company?.trim() || NO_COMPANY;
}

/** Sort a copy of the builds. The primary key is `created_at` (Newest) or the
 *  score (Score); `dir` flips it, so Newest asc reads oldest-first and Score
 *  asc reads weakest-first with missing scores (worst of all) at the very
 *  front. The tiebreak stays fixed regardless of direction, newest then id, so
 *  equal-key builds hold one stable order either way. */
function sortBuilds(
  builds: readonly BuildSummary[],
  sort: SortBy,
  dir: SortDir,
): BuildSummary[] {
  const arr = [...builds];
  const sign = dir === 'asc' ? -1 : 1;
  arr.sort((a, b) => {
    // Both forms are "descending-natural" (bigger first); `sign` reverses them.
    const primary = sort === 'score' ? scoreOf(b) - scoreOf(a) : timeOf(b) - timeOf(a);
    // A NaN primary (two missing scores subtracted) is falsy and falls through.
    if (primary) return sign * primary;
    return timeOf(b) - timeOf(a) || cmpId(a, b);
  });
  return arr;
}

/** Cluster builds under company headers, each group internally sorted, the
 *  groups themselves ordered by their newest build. A blank company falls under
 *  a single "No company" header. */
function groupByCompany(
  builds: readonly BuildSummary[],
  sort: SortBy,
  dir: SortDir,
): BuildSection[] {
  const map = new Map<string, BuildSummary[]>();
  for (const b of builds) {
    const key = groupKey(b);
    const arr = map.get(key);
    if (arr) arr.push(b);
    else map.set(key, [b]);
  }
  const groups = [...map.entries()].map(([company, list]) => ({
    company,
    builds: sortBuilds(list, sort, dir),
  }));
  groups.sort((a, b) => newestTime(b.builds) - newestTime(a.builds));
  return groups;
}

/** A build's score for sorting: its numeric score, or -Infinity when it lacks
 *  one (missing/NaN) so those builds sort at the weak end whatever the order. */
function scoreOf(b: BuildSummary): number {
  return typeof b.score === 'number' && !Number.isNaN(b.score) ? b.score : -Infinity;
}

/** A stable id comparison, the final tiebreak so any two builds have exactly
 *  one order. Ids are zero-padded numeric strings, so a lexical compare matches
 *  their numeric order. */
function cmpId(a: BuildSummary, b: BuildSummary): number {
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

function timeOf(b: BuildSummary): number {
  const t = new Date(b.created_at).getTime();
  return Number.isNaN(t) ? 0 : t;
}

/** The newest `created_at` across a group, for ordering the groups themselves. */
function newestTime(builds: readonly BuildSummary[]): number {
  return builds.reduce((max, b) => Math.max(max, timeOf(b)), -Infinity);
}

function readGroup(): boolean {
  // Default ON: only an explicit '0' turns grouping off.
  return readLs(GROUP_KEY) !== '0';
}

function readSort(): SortBy {
  return readLs(SORT_KEY) === 'score' ? 'score' : 'newest';
}

function readDir(): SortDir {
  // Default desc (newest first): only an explicit 'asc' flips it.
  return readLs(DIR_KEY) === 'asc' ? 'asc' : 'desc';
}

function readCollapsed(): Set<string> {
  const raw = readLs(COLLAPSED_KEY);
  if (!raw) return new Set();
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? new Set(parsed.filter((v): v is string => typeof v === 'string')) : new Set();
  } catch {
    return new Set();
  }
}

/** The server's error message for a failed remove, if it sent one, else a plain
 *  fallback. An `HttpErrorResponse` carries the parsed JSON body on `.error`,
 *  which for this API is `{ error: { kind, message } }`. */
function removeErrorMessage(err: unknown): string {
  const body = (err as { error?: { error?: { message?: unknown } } })?.error?.error?.message;
  if (typeof body === 'string' && body.length > 0) return body;
  const message = (err as { message?: unknown })?.message;
  return typeof message === 'string' && message.length > 0 ? message : 'could not remove the build';
}

function readLs(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeLs(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private mode / storage disabled: the preference just doesn't persist.
  }
}
