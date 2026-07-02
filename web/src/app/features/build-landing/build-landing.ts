import { ChangeDetectionStrategy, Component, computed, effect, inject } from '@angular/core';
import { Router, RouterLink } from '@angular/router';

import { BuildsStore } from '../../services/builds-store';

/** The `/` landing. There is no separate "overview" screen — a build is a
 *  build, rendered by one component (the tailoring workspace). As soon as the
 *  build list settles, this redirects to the newest build's workspace
 *  (`/build/:id/tailor`). With no builds yet, it shows a first-run prompt. */
@Component({
  selector: 'app-build-landing',
  standalone: true,
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (store.loading()) {
      <div class="panel muted">Loading builds…</div>
    } @else if (store.error(); as e) {
      <div class="panel">
        <b>Couldn’t load builds.</b>
        <p class="muted">{{ e }}</p>
      </div>
    } @else if (store.builds().length === 0) {
      <div class="panel first-run">
        <h2>No builds yet</h2>
        <p class="muted">
          Start one with <a routerLink="/new">New Build</a> to tailor a résumé to a job description.
        </p>
      </div>
    }
  `,
  styles: `
    :host { display: block; }
    .panel { border: 1px solid var(--border); border-radius: var(--radius); padding: 28px; background: var(--surface); }
    .first-run h2 { font-family: var(--font-display); margin: 0 0 8px; }
    .muted { color: var(--muted); }
  `,
})
export class BuildLanding {
  protected readonly store = inject(BuildsStore);
  private readonly router = inject(Router);

  private readonly newest = computed(() => this.store.builds()[0]?.id ?? null);

  constructor() {
    // Once the list has loaded, hand off to the newest build's workspace.
    // `replaceUrl` so `/` doesn't linger in history and trap the back button.
    effect(() => {
      if (this.store.loading()) return;
      const id = this.newest();
      if (id) {
        void this.router.navigate(['/build', id, 'tailor'], { replaceUrl: true });
      }
    });
  }
}
