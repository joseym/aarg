import { Component, computed, effect, inject, signal } from '@angular/core';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { map } from 'rxjs';

import { BuildsStore } from '../../services/builds-store';
import { ApiService } from '../../services/api.service';
import { WasmService } from '../../services/wasm.service';
import { CoverageMap } from '../../shared/coverage-map';
import { ViewToggle } from '../../shared/view-toggle';
import type {
  BuildDetail,
  VariantPayload,
} from '../../models';

/** The shape `weighted_coverage` actually returns — a weighted fraction plus the
 *  matched/total counts and a per-importance breakdown. (The `WasmService`
 *  method is typed `AtsReport`, but the runtime export emits this instead, so we
 *  cast to it here.) `score` is a 0..1 fraction; multiply by 100 for a percent. */
interface TierCount {
  matched: number;
  total: number;
}
interface WeightedCoverage {
  score: number;
  matched: number;
  total: number;
  by_importance: { critical: TierCount; required: TierCount; preferred: TierCount };
}

type Tier = 'high' | 'mid' | 'low';

const MONTHS = [
  'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
  'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec',
];

/** Build overview (`/`): the "Requirements coverage" build view. Loads one
 *  build's artifact bundle (`GET /api/builds/:id`) — the newest by default, or a
 *  `:id` route param when one is present — then renders the build header, the
 *  weighted-coverage score (computed by the wasm core), and the requirement ↔
 *  evidence coverage table, with a Coverage-map / Final-preview toggle. */
@Component({
  selector: 'app-build-overview',
  imports: [RouterLink, CoverageMap, ViewToggle],
  templateUrl: './build-overview.html',
  styleUrl: './build-overview.css',
})
export class BuildOverview {
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly api = inject(ApiService);
  private readonly wasm = inject(WasmService);
  protected readonly store = inject(BuildsStore);

  /** An explicit `:id` from the route, or null at `/` (then we show the newest). */
  private readonly routeId = toSignal(
    this.route.paramMap.pipe(map((p) => p.get('id'))),
    { initialValue: null as string | null },
  );

  /** Which build to show: the route's `:id`, else the newest indexed build. */
  protected readonly targetId = computed(
    () => this.routeId() ?? this.store.builds()[0]?.id ?? null,
  );

  protected readonly detail = signal<BuildDetail | null>(null);
  protected readonly coverage = signal<WeightedCoverage | null>(null);
  protected readonly loading = signal(true);
  protected readonly error = signal<string | null>(null);

  /** Animated count-up value for the headline number (0..100, whole percent). */
  protected readonly displayScore = signal(0);

  protected readonly view = signal<'coverage' | 'preview'>('coverage');

  private readonly reduceMotion =
    typeof matchMedia !== 'undefined' &&
    matchMedia('(prefers-reduced-motion: reduce)').matches;

  /** Ignore responses from a build load that a newer one has superseded. */
  private seq = 0;

  constructor() {
    effect(() => {
      const id = this.targetId();
      if (id) {
        this.loadBuild(id);
      } else if (!this.store.loading()) {
        // The store settled with no builds — nothing to show.
        this.loading.set(false);
        this.detail.set(null);
      }
    });
  }

  // ── derived view state ────────────────────────────────────────────────

  /** No builds exist at all (store settled empty, and no explicit id was asked). */
  protected readonly noBuilds = computed(
    () =>
      !this.store.loading() &&
      this.store.builds().length === 0 &&
      !this.routeId(),
  );

  protected readonly tier = computed<Tier>(() => bandTier(this.coverage()?.score ?? 0));

  /** The variant payload to project into the preview. Prefer the human variant,
   *  but most builds are rendered ATS-only, so fall back to the ATS payload —
   *  it's the same `VariantPayload` shape (unlike `canonical`, a TailoredResume,
   *  which would not render correctly here). */
  protected readonly previewDoc = computed<VariantPayload | null>(() => {
    const d = this.detail();
    const doc = d?.human_payload ?? d?.ats_payload ?? null;
    if (!doc || !Array.isArray(doc.roles)) return null;
    return doc;
  });

  protected readonly status = computed<'Tailored' | 'Exported'>(() =>
    (this.detail()?.pdfs?.length ?? 0) > 0 ? 'Exported' : 'Tailored',
  );

  protected readonly provenance = computed(() => {
    const d = this.detail();
    const meta = d?.meta;
    if (!meta) return null;
    const tokens =
      (meta.tailor_usage?.input_tokens ?? 0) + (meta.tailor_usage?.output_tokens ?? 0);
    return {
      created: formatStamp(meta.created_at),
      model: meta.model,
      tokens: tokens.toLocaleString(),
      template: meta.template,
    };
  });

  /** company · location/remote for the header meta line. */
  protected readonly locationLine = computed(() => {
    const jd = this.detail()?.jd;
    if (!jd) return '';
    if (jd.location) return jd.location;
    if (jd.remote && jd.remote !== 'unspecified' && jd.remote !== 'onsite') {
      return jd.remote === 'remote' ? 'Remote' : jd.remote;
    }
    return '';
  });

  // ── interactions ──────────────────────────────────────────────────────

  protected setView(mode: 'coverage' | 'preview'): void {
    this.view.set(mode);
  }

  /** A coverage-map row's action → deep-link into the tailoring screen, carrying
   *  the requirement name and match intent (matched / semantic / gap). */
  protected onCovAct(e: { name: string; intent: 'matched' | 'semantic' | 'gap' }): void {
    this.router.navigate(this.tailorLink(), {
      queryParams: { focus: e.name, intent: e.intent },
    });
  }

  protected retry(): void {
    const id = this.targetId();
    if (id) this.loadBuild(id);
  }

  protected tailorLink(): unknown[] {
    return ['/build', this.targetId(), 'tailor'];
  }

  // ── loading + coverage ────────────────────────────────────────────────

  private loadBuild(id: string): void {
    const token = ++this.seq;
    this.loading.set(true);
    this.error.set(null);
    this.api.getBuild(id).subscribe({
      next: (detail) => {
        if (token !== this.seq) return;
        this.detail.set(detail);
        this.view.set('coverage');
        this.computeCoverage(detail, token);
        this.loading.set(false);
      },
      error: (err) => {
        if (token !== this.seq) return;
        this.error.set(err?.message ?? 'Could not load this build.');
        this.loading.set(false);
      },
    });
  }

  private async computeCoverage(detail: BuildDetail, token: number): Promise<void> {
    const jd = detail.jd;
    const gap = detail.gap_report;
    if (!jd || !gap) {
      this.coverage.set(null);
      this.displayScore.set(0);
      return;
    }
    try {
      const cov = (await this.wasm.weightedCoverage(
        gap,
        jd,
      )) as unknown as WeightedCoverage;
      if (token !== this.seq) return;
      this.coverage.set(cov);
      this.animateScore(cov.score);
    } catch {
      if (token !== this.seq) return;
      this.coverage.set(null);
      this.displayScore.set(0);
    }
  }

  private animateScore(score01: number): void {
    const target = Math.round(score01 * 100);
    if (this.reduceMotion) {
      this.displayScore.set(target);
      return;
    }
    const duration = 640;
    const start = performance.now();
    const ease = (p: number): number => 1 - Math.pow(1 - p, 3);
    const step = (now: number): void => {
      const p = Math.min(1, (now - start) / duration);
      this.displayScore.set(Math.round(target * ease(p)));
      if (p < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }

  // ── preview helpers (kept lightweight) ────────────────────────────────

  protected fmtYM(ym: string | null | undefined): string {
    if (!ym) return 'Present';
    const [y, m] = ym.split('-');
    const idx = Number(m) - 1;
    return idx >= 0 && idx < 12 ? `${MONTHS[idx]} ${y}` : ym;
  }

  protected contactLine(p: VariantPayload): string {
    const c = p.contact;
    return [c.location, c.email, c.phone, ...(c.links ?? []).map((l) => l.url)]
      .filter(Boolean)
      .join('  ·  ');
  }
}

/** Weighted-coverage colour band: green ≥ .8, amber ≥ .6, red below. */
function bandTier(score01: number): Tier {
  if (score01 >= 0.8) return 'high';
  if (score01 >= 0.6) return 'mid';
  return 'low';
}

/** ISO timestamp → "Jun 25, 2026 · 14:22" (local time). Falls back to the raw
 *  string if it can't be parsed. */
function formatStamp(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  return `${MONTHS[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()} · ${hh}:${mm}`;
}
