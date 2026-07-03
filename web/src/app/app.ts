import { Component, OnInit, inject, signal } from '@angular/core';
import { RouterOutlet } from '@angular/router';

import { Topbar } from './shell/topbar/topbar';
import { Sidebar } from './shell/sidebar/sidebar';
import { BuildsStore } from './services/builds-store';
import { WasmService } from './services/wasm.service';
import { CopilotOverlay } from './shared/copilot-host';

const SIDEBAR_COLLAPSED_KEY = 'aarg.sidebar-collapsed';

/** Reads the persisted desktop collapse choice. Guarded for contexts where
 *  `localStorage` doesn't exist. */
function loadSidebarCollapsed(): boolean {
  if (typeof localStorage === 'undefined') return false;
  return localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1';
}

/** App shell: the sticky topbar over a two-column grid of the recent-builds
 *  sidebar and the routed main panel. At ≤1080px the sidebar becomes a drawer
 *  toggled from the topbar hamburger, with a dismiss scrim. Above 1080px the
 *  sidebar column can instead be collapsed to a slim rail via a control on
 *  the sidebar itself; that choice persists across sessions. */
@Component({
  selector: 'app-root',
  imports: [RouterOutlet, Topbar, Sidebar, CopilotOverlay],
  templateUrl: './app.html',
  styleUrl: './app.css',
  host: { '(document:keydown.escape)': 'closeNav()' },
})
export class App implements OnInit {
  private readonly store = inject(BuildsStore);
  private readonly wasm = inject(WasmService);

  /** Drawer open state (only meaningful ≤1080px). */
  protected readonly navOpen = signal(false);

  /** Dismiss the mobile drawer on Escape. A no-op when it is already closed, so
   *  the key stays free for anything else listening on wider layouts. */
  protected closeNav(): void {
    if (this.navOpen()) this.navOpen.set(false);
  }

  /** Desktop sidebar collapse (only meaningful above 1080px; ignored by the
   *  ≤1080px drawer, which always opens full-width regardless). */
  protected readonly collapsed = signal(loadSidebarCollapsed());

  protected toggleCollapsed(): void {
    const next = !this.collapsed();
    this.collapsed.set(next);
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(SIDEBAR_COLLAPSED_KEY, next ? '1' : '0');
    }
  }

  ngOnInit(): void {
    this.store.load();
    // Warm the deterministic wasm core in the background so the first tailoring
    // action is instant. The .wasm is a same-origin static asset, so this needs
    // no backend; failures are non-fatal (the core loads on demand regardless).
    this.wasm.normalizeDashes('').catch(() => {});
  }
}
