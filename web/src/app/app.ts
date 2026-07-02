import { Component, OnInit, inject, signal } from '@angular/core';
import { RouterOutlet } from '@angular/router';

import { Topbar } from './shell/topbar/topbar';
import { Sidebar } from './shell/sidebar/sidebar';
import { BuildsStore } from './services/builds-store';
import { WasmService } from './services/wasm.service';
import { CopilotOverlay } from './shared/copilot-host';

/** App shell: the sticky topbar over a two-column grid of the recent-builds
 *  sidebar and the routed main panel. At ≤1080px the sidebar becomes a drawer
 *  toggled from the topbar hamburger, with a dismiss scrim. */
@Component({
  selector: 'app-root',
  imports: [RouterOutlet, Topbar, Sidebar, CopilotOverlay],
  templateUrl: './app.html',
  styleUrl: './app.css',
})
export class App implements OnInit {
  private readonly store = inject(BuildsStore);
  private readonly wasm = inject(WasmService);

  /** Drawer open state (only meaningful ≤1080px). */
  protected readonly navOpen = signal(false);

  ngOnInit(): void {
    this.store.load();
    // Warm the deterministic wasm core in the background so the first tailoring
    // action is instant. The .wasm is a same-origin static asset, so this needs
    // no backend; failures are non-fatal (the core loads on demand regardless).
    this.wasm.normalizeDashes('').catch(() => {});
  }
}
