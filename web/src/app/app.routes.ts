import { Routes } from '@angular/router';

/** One build screen. `/` is a thin landing that redirects to the newest
 *  build's workspace; `/build/:id/tailor` is the single component that renders
 *  any build (latest or selected). There is no separate overview. */
export const routes: Routes = [
  {
    path: '',
    title: 'AARG · Builds',
    loadComponent: () =>
      import('./features/build-landing/build-landing').then((m) => m.BuildLanding),
  },
  {
    path: 'new',
    title: 'AARG · New Build',
    loadComponent: () => import('./features/new-build/new-build').then((m) => m.NewBuild),
  },
  {
    path: 'build/:id/tailor',
    title: 'AARG · Tailoring',
    loadComponent: () =>
      import('./features/tailoring-workspace/tailoring-workspace').then(
        (m) => m.TailoringWorkspace,
      ),
  },
  { path: '**', redirectTo: '' },
];
