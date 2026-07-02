import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  input,
  signal,
  untracked,
} from '@angular/core';

type Tier = 'high' | 'mid' | 'low';

/** The headline weighted-coverage score, rendered identically wherever it
 *  appears — the build overview and the tailoring workspace both use this, so
 *  the "large animated number" reads the same on every screen. `compact` scales
 *  it down for the workspace context bar while keeping the same treatment
 *  (display serif, tier colour, count-up, meter). `score` is a 0..1 fraction. */
@Component({
  selector: 'app-coverage-score',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="coverage" [attr.data-tier]="tier()" [class.compact]="compact()">
      <div class="cov-num">
        <span class="big num">{{ display() }}</span><span class="pct">%</span>
      </div>
      <div class="cov-label">weighted coverage</div>
      <div class="cov-meter"><i [style.width.%]="display()"></i></div>
      <div class="cov-sub"><b>{{ matched() }}</b> of <b>{{ total() }}</b> requirements matched</div>
    </div>
  `,
  styles: [
    `
      .cov-num {
        font-family: var(--font-display);
        font-weight: 600;
        letter-spacing: -0.03em;
        line-height: 0.9;
        color: var(--accent);
      }
      .cov-num .big {
        font-size: clamp(64px, 8vw, 92px);
        font-variant-numeric: tabular-nums;
      }
      .cov-num .pct {
        font-size: 0.42em;
        color: var(--muted);
        margin-left: 2px;
      }
      .cov-label {
        font-family: var(--font-mono);
        font-size: 10.5px;
        letter-spacing: 0.14em;
        text-transform: uppercase;
        color: var(--faint);
        margin-top: 4px;
      }
      .cov-meter {
        height: 4px;
        border-radius: 999px;
        background: var(--surface-2);
        margin-top: 14px;
        overflow: hidden;
      }
      .cov-meter i {
        display: block;
        height: 100%;
        background: var(--accent);
        border-radius: 999px;
        transition: width 0.6s cubic-bezier(0.2, 0.7, 0.2, 1);
      }
      .cov-sub {
        font-family: var(--font-mono);
        font-size: 11.5px;
        color: var(--muted);
        margin-top: 10px;
      }
      .cov-sub b {
        color: var(--fg);
      }
      .coverage[data-tier='high'] .cov-num {
        color: var(--success);
      }
      .coverage[data-tier='high'] .cov-meter i {
        background: var(--success);
      }
      .coverage[data-tier='mid'] .cov-num {
        color: var(--warn);
      }
      .coverage[data-tier='mid'] .cov-meter i {
        background: var(--warn);
      }
      .coverage[data-tier='low'] .cov-num {
        color: var(--danger);
      }
      .coverage[data-tier='low'] .cov-meter i {
        background: var(--danger);
      }

      /* Compact: the workspace context bar. Same treatment, smaller. */
      .coverage.compact .cov-num .big {
        font-size: clamp(40px, 6vw, 54px);
      }
      .coverage.compact .cov-label {
        margin-top: 2px;
      }
      .coverage.compact .cov-meter {
        margin-top: 8px;
        max-width: 200px;
      }
      .coverage.compact .cov-sub {
        margin-top: 6px;
      }

      @media (prefers-reduced-motion: reduce) {
        .cov-meter i {
          transition: none !important;
        }
      }
    `,
  ],
})
export class CoverageScore {
  /** 0..1 weighted-coverage fraction. */
  readonly score = input<number>(0);
  readonly matched = input<number>(0);
  readonly total = input<number>(0);
  readonly compact = input<boolean>(false);

  /** Count-up value shown in the number and the meter (0..100, whole percent). */
  protected readonly display = signal(0);

  protected readonly tier = computed<Tier>(() => {
    const s = this.score();
    return s >= 0.8 ? 'high' : s >= 0.6 ? 'mid' : 'low';
  });

  private readonly reduceMotion =
    typeof matchMedia !== 'undefined' && matchMedia('(prefers-reduced-motion: reduce)').matches;

  constructor() {
    // Re-run only when the score changes; the animation reads `display`
    // untracked so it doesn't feed back into itself.
    effect(() => {
      const target = Math.round(this.score() * 100);
      untracked(() => this.animate(target));
    });
  }

  private animate(target: number): void {
    if (this.reduceMotion) {
      this.display.set(target);
      return;
    }
    const from = this.display();
    const duration = 640;
    const start = performance.now();
    const ease = (p: number): number => 1 - Math.pow(1 - p, 3);
    const step = (now: number): void => {
      const p = Math.min(1, (now - start) / duration);
      this.display.set(Math.round(from + (target - from) * ease(p)));
      if (p < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }
}
