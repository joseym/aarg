import { Component, effect, inject, input, signal } from '@angular/core';
import { RouterLink } from '@angular/router';

import { ApiService } from '../../services/api.service';
import type { BuildDetail } from '../../models';

/** Tailoring workspace (`/build/:id/tailor`): the per-build screen. This wave
 *  ships a placeholder that resolves the `:id` route param, fetches the build's
 *  artifact bundle (`GET /api/builds/:id`), and renders a header + summary — so
 *  the parameterised route and detail call are proven end to end. The full
 *  adversarial-loop workspace lands in a later wave. */
@Component({
  selector: 'app-tailoring-workspace',
  imports: [RouterLink],
  template: `
    <a class="back" routerLink="/">← All builds</a>

    @if (loading()) {
      <div class="panel muted">Loading build {{ id() }}…</div>
    } @else if (error()) {
      <div class="panel">
        <b>Couldn't load build {{ id() }}.</b>
        <p class="muted">{{ error() }}</p>
      </div>
    } @else if (detail(); as d) {
      <header class="head anim-up">
        <div class="kicker"><span class="dot"></span> Tailoring Workspace · Build {{ d.build_id }}</div>
        <h1>{{ d.jd?.title || d.canonical?.target_title || 'Untitled build' }}</h1>
        @if (d.jd?.company) {
          <div class="co">{{ d.jd?.company }}</div>
        }
      </header>

      <section class="stat-row anim-up">
        <div class="stat">
          <span class="stat-num">{{ pct(d.ats_report?.coverage) }}<i>%</i></span>
          <span class="stat-label">ATS Coverage</span>
        </div>
        <div class="stat">
          <span class="stat-num">{{ round100(d.adversarial_report?.overall_score) }}</span>
          <span class="stat-label">Review Score</span>
        </div>
        <div class="stat">
          <span class="stat-num">{{ d.adversarial_report?.objections?.length ?? 0 }}</span>
          <span class="stat-label">Objections</span>
        </div>
        <div class="stat">
          <span class="stat-num">{{ d.pdfs.length }}</span>
          <span class="stat-label">Rendered PDFs</span>
        </div>
      </section>

      <div class="panel note anim-up">
        The full tailoring workspace — the adversarial loop, objection triage,
        and the ATS / Human variant panes — arrives in the next wave. The data
        it needs is already wired: this build carries
        {{ d.jd?.required_skills?.length ?? 0 }} required skills,
        {{ d.gap_report?.matched?.length ?? 0 }} matched and
        {{ d.gap_report?.unknown?.length ?? 0 }} unresolved.
      </div>
    }
  `,
  styles: `
    :host { display: block; }
    .back {
      display: inline-block; font-family: var(--font-mono); font-size: 12px;
      color: var(--muted); text-decoration: none; margin-bottom: 22px;
    }
    .back:hover { color: var(--fg); }

    .head { padding-bottom: 22px; border-bottom: 1.5px solid var(--border-ink); margin-bottom: 26px; }
    .kicker {
      font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.12em;
      text-transform: uppercase; color: var(--accent);
      display: flex; align-items: center; gap: 10px; margin-bottom: 12px;
    }
    .kicker .dot { width: 5px; height: 5px; border-radius: 50%; background: var(--accent); }
    h1 { font-size: clamp(26px, 3vw, 38px); line-height: 1.05; letter-spacing: -0.015em; }
    .co { color: var(--muted); font-size: 15px; margin-top: 8px; }

    .stat-row { display: flex; flex-wrap: wrap; gap: 40px; margin-bottom: 30px; }
    .stat { display: flex; flex-direction: column; gap: 3px; }
    .stat-num {
      font-family: var(--font-display); font-weight: 600; font-size: 40px;
      letter-spacing: -0.03em; line-height: 0.9; color: var(--accent);
    }
    .stat-num i { font-size: 0.4em; color: var(--muted); font-style: normal; }
    .stat-label {
      font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.12em;
      text-transform: uppercase; color: var(--faint);
    }

    .panel {
      background: var(--surface); border: 1px solid var(--border);
      border-radius: var(--radius-lg); padding: 22px; max-width: 68ch;
      display: flex; flex-direction: column; gap: 10px; align-items: flex-start;
    }
    .note { color: var(--muted); line-height: 1.6; }
    .muted { color: var(--muted); }
  `,
})
export class TailoringWorkspace {
  private readonly api = inject(ApiService);

  /** Bound from the `:id` route segment via `withComponentInputBinding`. */
  readonly id = input.required<string>();

  protected readonly detail = signal<BuildDetail | null>(null);
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);

  constructor() {
    // Refetch whenever the route id changes.
    effect(() => {
      const id = this.id();
      this.loading.set(true);
      this.error.set(null);
      this.detail.set(null);
      this.api.getBuild(id).subscribe({
        next: (d) => {
          this.detail.set(d);
          this.loading.set(false);
        },
        error: (err) => {
          this.error.set(err?.message ?? 'request failed');
          this.loading.set(false);
        },
      });
    });
  }

  protected pct(v: number | undefined): number {
    return Math.round((v ?? 0) * 100);
  }
  protected round100(v: number | undefined): number {
    return Math.round((v ?? 0) * 100);
  }
}
