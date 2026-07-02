import { Component, inject } from '@angular/core';
import { RouterLink } from '@angular/router';

import { BuildsStore } from '../../services/builds-store';
import type { BuildSummary } from '../../models';

/** Build overview (`/`): the landing panel. Lists every build from a live
 *  `GET /api/builds` (via the shared store) as cards. Placeholder for the full
 *  overview screen a later wave builds; here it proves routing + the shell + a
 *  real data call all render. */
@Component({
  selector: 'app-build-overview',
  imports: [RouterLink],
  template: `
    <header class="overview-head anim-up">
      <div class="oh-kicker"><span class="dot"></span> Build Overview</div>
      <h1>Your tailored resumes</h1>
      <p class="oh-sub">
        Every build AARG has produced, newest first. Pick one to open its
        tailoring workspace.
      </p>
      <!-- Tailwind utilities (incl. theme-mapped bg-surface / text-muted) sit
           alongside the component's own CSS, proving the token layer is
           reachable both as utilities and as CSS custom properties. -->
      <span
        class="mt-4 inline-flex items-center gap-2 rounded-lg border border-[var(--border)] bg-surface px-3 py-1 font-mono text-xs text-muted"
      >
        <span class="inline-block size-1.5 rounded-full bg-accent"></span>
        {{ store.builds().length }} builds indexed
      </span>
    </header>

    @if (store.loading()) {
      <div class="panel muted">Loading builds…</div>
    } @else if (store.error()) {
      <div class="panel">
        <b>Couldn't reach the API.</b>
        <p class="muted">
          Start the backend with <code>aarg serve</code> (listening on
          <code>127.0.0.1:8787</code>), then reload. In dev, <code>/api</code>
          is proxied there automatically.
        </p>
        <button class="btn btn-ghost" type="button" (click)="store.load()">Retry</button>
      </div>
    } @else if (store.filtered().length === 0) {
      <div class="panel muted">
        No builds match. Clear the filter, or start one with <b>New Build</b>.
      </div>
    } @else {
      <div class="grid">
        @for (b of store.filtered(); track b.id) {
          <a class="card anim-up" [routerLink]="['/build', b.id, 'tailor']">
            <div class="card-top">
              <div>
                <div class="card-title">{{ b.title }}</div>
                @if (b.company) {
                  <div class="card-co">{{ b.company }}</div>
                }
              </div>
              <span class="score-badge" [attr.data-tier]="tier(b)">{{ pct(b.score) }}</span>
            </div>

            <div class="stats">
              <div class="stat">
                <span class="stat-num">{{ pct(b.coverage) }}<i>%</i></span>
                <span class="stat-label">Coverage</span>
              </div>
              <div class="stat">
                <span class="stat-num">{{ pct(b.review_score) }}</span>
                <span class="stat-label">Review</span>
              </div>
              <div class="stat">
                <span class="stat-num">{{ b.objections }}</span>
                <span class="stat-label">Objections</span>
              </div>
            </div>

            <div class="card-foot">
              <span class="tmpl">{{ b.template }}</span>
              <span class="date">{{ b.created_at }}</span>
            </div>
          </a>
        }
      </div>
    }
  `,
  styles: `
    :host { display: block; }
    .overview-head { padding-bottom: 24px; border-bottom: 1.5px solid var(--border-ink); margin-bottom: 28px; }
    .oh-kicker {
      font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.12em;
      text-transform: uppercase; color: var(--accent);
      display: flex; align-items: center; gap: 10px; margin-bottom: 12px;
    }
    .oh-kicker .dot { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); }
    h1 { font-size: clamp(28px, 3vw, 40px); line-height: 1.05; letter-spacing: -0.015em; }
    .oh-sub { color: var(--muted); margin-top: 12px; max-width: 60ch; }

    .grid {
      display: grid; gap: 16px;
      grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    }
    .card {
      display: flex; flex-direction: column; gap: 16px;
      padding: 20px; text-decoration: none; color: inherit;
      background: var(--surface); border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      transition: border-color 0.15s, box-shadow 0.2s, transform 0.15s cubic-bezier(0.2, 0.7, 0.2, 1);
    }
    .card:hover {
      border-color: color-mix(in oklch, var(--accent) 40%, var(--border));
      transform: translateY(-2px);
      box-shadow: 0 12px 40px -28px color-mix(in oklch, var(--fg) 70%, transparent);
    }
    .card-top { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
    .card-title { font-family: var(--font-display); font-size: 18px; line-height: 1.2; }
    .card-co { color: var(--muted); font-size: 13px; margin-top: 2px; }

    .score-badge {
      flex-shrink: 0; font-family: var(--font-mono); font-variant-numeric: tabular-nums;
      font-size: 12.5px; font-weight: 600; padding: 3px 8px; border-radius: 6px;
      border: 1px solid var(--border); color: var(--fg); background: var(--bg);
    }
    .score-badge[data-tier='high'] { color: var(--success); border-color: color-mix(in oklch, var(--success) 40%, var(--border)); }
    .score-badge[data-tier='mid']  { color: var(--warn); border-color: color-mix(in oklch, var(--warn) 40%, var(--border)); }
    .score-badge[data-tier='low']  { color: var(--danger); border-color: color-mix(in oklch, var(--danger) 40%, var(--border)); }

    .stats { display: flex; gap: 22px; }
    .stat { display: flex; flex-direction: column; gap: 2px; }
    .stat-num {
      font-family: var(--font-display); font-weight: 600; font-size: 24px;
      letter-spacing: -0.02em; color: var(--fg);
    }
    .stat-num i { font-size: 0.55em; color: var(--muted); font-style: normal; margin-left: 1px; }
    .stat-label {
      font-family: var(--font-mono); font-size: 10px; letter-spacing: 0.1em;
      text-transform: uppercase; color: var(--faint);
    }

    .card-foot {
      display: flex; align-items: center; justify-content: space-between;
      padding-top: 14px; border-top: 1px solid var(--border);
      font-family: var(--font-mono); font-size: 11.5px; color: var(--faint);
    }
    .tmpl { color: var(--muted); }

    .panel {
      background: var(--surface); border: 1px solid var(--border);
      border-radius: var(--radius-lg); padding: 24px; max-width: 60ch;
      display: flex; flex-direction: column; gap: 12px; align-items: flex-start;
    }
    .muted { color: var(--muted); }
    code { font-family: var(--font-mono); font-size: 0.9em; color: var(--accent); }
    .btn {
      display: inline-flex; align-items: center; height: 34px; padding: 0 14px;
      border-radius: var(--radius); border: 1px solid var(--border);
      background: transparent; color: var(--fg); font: inherit; font-size: 14px; cursor: pointer;
    }
    .btn-ghost:hover { border-color: var(--fg); }
  `,
})
export class BuildOverview {
  protected readonly store = inject(BuildsStore);

  protected pct(v: number): number {
    return Math.round(v * 100);
  }

  protected tier(b: BuildSummary): 'high' | 'mid' | 'low' {
    if (b.score >= 0.8) return 'high';
    if (b.score >= 0.6) return 'mid';
    return 'low';
  }
}
