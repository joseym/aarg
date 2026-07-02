import { Injectable, computed, inject, signal } from '@angular/core';

import { ApiService } from './api.service';
import type { BuildSummary } from '../models';

/** Shared, signal-backed cache of the build list. The sidebar and the overview
 *  both read it (so a single `GET /api/builds` feeds both), and the topbar
 *  writes the filter. Kept deliberately small — this wave only lists builds. */
@Injectable({ providedIn: 'root' })
export class BuildsStore {
  private readonly api = inject(ApiService);

  readonly builds = signal<BuildSummary[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  /** Free-text filter over title/company, driven by the topbar input. */
  readonly filter = signal('');

  /** Builds matching the current filter (case-insensitive, title + company). */
  readonly filtered = computed(() => {
    const q = this.filter().trim().toLowerCase();
    const all = this.builds();
    if (!q) return all;
    return all.filter(
      (b) =>
        b.title.toLowerCase().includes(q) ||
        b.company.toLowerCase().includes(q) ||
        b.target.toLowerCase().includes(q),
    );
  });

  /** Fetch the build list once (or on explicit refresh). */
  load(): void {
    this.loading.set(true);
    this.error.set(null);
    this.api.getBuilds().subscribe({
      next: (builds) => {
        this.builds.set(builds);
        this.loading.set(false);
      },
      error: (err) => {
        this.error.set(err?.message ?? 'could not reach aarg serve');
        this.loading.set(false);
      },
    });
  }
}
