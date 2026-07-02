import { Routes } from '@angular/router';

/** Two routes for this wave: the build overview (`/`) and the per-build
 *  tailoring workspace (`/build/:id/tailor`). Both are lazy standalone
 *  components; the full screens land in a later wave. */
export const routes: Routes = [
  {
    path: '',
    title: 'AARG — Builds',
    loadComponent: () =>
      import('./features/build-overview/build-overview').then((m) => m.BuildOverview),
  },
  {
    path: 'build/:id/tailor',
    title: 'AARG — Tailoring',
    loadComponent: () =>
      import('./features/tailoring-workspace/tailoring-workspace').then(
        (m) => m.TailoringWorkspace,
      ),
  },
  { path: '**', redirectTo: '' },
];
