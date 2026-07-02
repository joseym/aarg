import { Component, inject, output } from '@angular/core';
import { RouterLink } from '@angular/router';

import { BuildsStore } from '../../services/builds-store';

/** The sticky top bar: hamburger (mobile), AARG wordmark + mono kicker, the
 *  build filter input, and the New Build action. Geometry and tokens mirror the
 *  prototype export. */
@Component({
  selector: 'app-topbar',
  imports: [RouterLink],
  template: `
    <header class="topbar">
      <button class="hamburger" type="button" aria-label="Show recent builds" (click)="toggleNav.emit()">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M3 6h18M3 12h18M3 18h18" />
        </svg>
      </button>

      <div class="wordmark">
        <b>AARG</b>
        <span class="wm-sub">plunder the posting</span>
      </div>

      <label class="search">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <circle cx="11" cy="11" r="7" /><path d="m21 21-4.3-4.3" />
        </svg>
        <input
          type="search"
          placeholder="Filter builds by role or company…"
          autocomplete="off"
          [value]="store.filter()"
          (input)="onFilter($event)"
        />
      </label>

      <a class="btn btn-primary" routerLink="/new">
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2">
          <path d="M12 5v14M5 12h14" />
        </svg>
        New Build
      </a>
    </header>
  `,
  styles: `
    :host { display: contents; }
    .topbar {
      position: sticky; top: 0; z-index: 20;
      display: grid;
      grid-template-columns: 260px 1fr auto;
      align-items: center;
      gap: 24px;
      height: 60px;
      padding: 0 22px;
      background: color-mix(in oklch, var(--bg) 88%, transparent);
      backdrop-filter: blur(10px);
      border-bottom: 1px solid var(--border);
    }
    .hamburger {
      display: none; align-items: center; justify-content: center;
      width: 38px; height: 38px; flex-shrink: 0;
      border: 1px solid var(--border); border-radius: var(--radius);
      background: var(--surface); color: var(--fg); cursor: pointer;
    }
    .hamburger:hover { border-color: var(--fg); }
    .hamburger svg { width: 18px; height: 18px; }

    .wordmark { display: flex; align-items: baseline; gap: 10px; }
    .wordmark b {
      font-family: var(--font-display); font-size: 23px; font-weight: 600;
      letter-spacing: 0.02em; color: var(--fg);
    }
    .wm-sub {
      font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.14em;
      text-transform: uppercase; color: var(--faint);
    }

    .search { position: relative; max-width: 440px; width: 100%; display: block; }
    .search svg {
      position: absolute; left: 12px; top: 50%; transform: translateY(-50%);
      width: 15px; height: 15px; color: var(--faint);
    }
    .search input {
      width: 100%; height: 38px; padding: 0 14px 0 34px;
      border: 1px solid var(--border); border-radius: 999px;
      background: var(--surface); color: var(--fg); font: inherit; font-size: 14px;
    }
    .search input::placeholder { color: var(--faint); }
    .search input:focus {
      outline: none; border-color: var(--accent);
      box-shadow: 0 0 0 3px var(--accent-soft);
    }

    .btn {
      display: inline-flex; align-items: center; gap: 8px;
      height: 38px; padding: 0 16px;
      border-radius: var(--radius); border: 1px solid transparent;
      font-size: 14px; font-weight: 500; letter-spacing: -0.005em;
      text-decoration: none; cursor: pointer;
      transition: background 0.15s, border-color 0.15s, transform 0.05s;
    }
    .btn:active { transform: translateY(1px); }
    .btn-primary {
      background: var(--accent); color: oklch(98% 0.01 250); border-color: var(--accent);
    }
    .btn-primary:hover { background: color-mix(in oklch, var(--accent) 88%, black); }

    @media (max-width: 1080px) {
      .topbar {
        display: flex; flex-wrap: wrap; align-items: center;
        gap: 10px 12px; height: auto; padding: 10px 14px;
      }
      .hamburger { display: inline-flex; order: 0; }
      .wordmark { order: 1; }
      .wm-sub { display: none; }
      .btn-primary { order: 2; margin-left: auto; height: 34px; padding: 0 13px; }
      .search { order: 3; flex-basis: 100%; max-width: none; }
    }
  `,
})
export class Topbar {
  protected readonly store = inject(BuildsStore);
  readonly toggleNav = output<void>();

  protected onFilter(event: Event): void {
    this.store.filter.set((event.target as HTMLInputElement).value);
  }
}
