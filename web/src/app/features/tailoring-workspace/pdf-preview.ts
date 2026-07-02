import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  effect,
  inject,
  input,
  output,
  signal,
} from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { DomSanitizer, type SafeResourceUrl } from '@angular/platform-browser';

import { ApiService } from '../../services/api.service';
import type { VariantPayload } from '../../models';

/** The honest mirror of the Typst templates: the *actual* rendered PDF, shown
 *  in an `<iframe>`. The editable HTML preview can never reproduce Typst's
 *  layout, so "Pixel-perfect" mode renders the payload through
 *  `POST /api/render` (the same Typst path a Download PDF uses) and displays the
 *  returned blob.
 *
 *  Blob lifecycle: every render's blob becomes an object URL via
 *  `URL.createObjectURL`; each `(variant, template)` pair's URL is cached so
 *  toggling the template picker back and forth doesn't re-hit Typst. The whole
 *  cache is revoked when the *payload* changes (a new render is required) and on
 *  destroy — so no object URL outlives the DOM that points at it. */
@Component({
  selector: 'app-pdf-preview',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="pdfwrap">
      @if (url(); as u) {
        <iframe class="pdf" [src]="u" title="Pixel-perfect PDF preview of your résumé"></iframe>
      }
      @if (rendering()) {
        <div class="pv-status" role="status">
          <span class="spin" aria-hidden="true"></span> Rendering…
        </div>
      } @else if (!url()) {
        <div class="pv-status muted">No preview to render.</div>
      }
    </div>
  `,
  styles: `
    :host { display: block; }
    .pdfwrap { position: relative; }
    .pdf {
      display: block;
      width: 100%;
      height: 78vh;
      border: 1px solid var(--border);
      border-radius: var(--radius-lg);
      background: var(--surface);
    }
    .pv-status {
      display: inline-flex; align-items: center; gap: 8px;
      position: absolute; top: 12px; left: 12px;
      font-family: var(--font-mono); font-size: 12px;
      padding: 6px 11px; border-radius: 999px;
      border: 1px solid var(--border); background: var(--surface);
      color: var(--muted); box-shadow: 0 2px 8px -4px color-mix(in oklch, var(--fg) 30%, transparent);
    }
    .pv-status.muted { position: static; box-shadow: none; }
    .spin { width: 11px; height: 11px; border-radius: 50%; border: 2px solid var(--border); border-top-color: var(--accent); animation: pv-spin 0.7s linear infinite; }
    @media (prefers-reduced-motion: reduce) { .spin { animation: none; } }
    @keyframes pv-spin { to { transform: rotate(360deg); } }
  `,
})
export class PdfPreview {
  private readonly api = inject(ApiService);
  private readonly sanitizer = inject(DomSanitizer);
  private readonly destroyRef = inject(DestroyRef);

  readonly variant = input.required<string>();
  readonly payload = input.required<VariantPayload>();
  /** The bare template name to render (e.g. `modern`), chosen by the picker. */
  readonly template = input.required<string>();
  /** Emitted with the raw error when a render fails — the host toasts it. */
  readonly error = output<unknown>();

  protected readonly rendering = signal(false);
  protected readonly url = signal<SafeResourceUrl | null>(null);

  /** Cached object URLs keyed by `${variant}::${template}`, valid for the
   *  current payload only. Revoked wholesale when the payload changes. */
  private readonly cache = new Map<string, { blobUrl: string; safe: SafeResourceUrl }>();
  private cachedPayload: VariantPayload | null = null;
  /** Monotonic request id so a slow render that's been superseded is discarded
   *  instead of clobbering a newer one (and never leaks its blob). */
  private reqId = 0;

  constructor() {
    this.destroyRef.onDestroy(() => this.clearCache());
    effect(() => {
      const variant = this.variant();
      const payload = this.payload();
      const template = this.template();
      this.renderFor(variant, payload, template);
    });
  }

  private renderFor(variant: string, payload: VariantPayload, template: string): void {
    // A payload change invalidates every cached render — the facts differ now.
    if (payload !== this.cachedPayload) {
      this.clearCache();
      this.cachedPayload = payload;
      this.url.set(null);
    }

    const key = `${variant}::${template}`;
    const hit = this.cache.get(key);
    if (hit) {
      // A cache hit ALSO supersedes: bump `reqId` so an older in-flight render
      // for a different key can't resolve after this and clobber the iframe
      // (picker showing A while the PDF silently swaps to B). The stale
      // response fails the `id === reqId` check below and is dropped.
      this.reqId++;
      this.url.set(hit.safe);
      this.rendering.set(false);
      return;
    }

    // Cache miss: render it. Keep any current iframe visible (a template switch
    // leaves the old one up) while the "Rendering…" chip shows the POST is live.
    const id = ++this.reqId;
    this.rendering.set(true);
    this.api
      .render(variant, payload, template || undefined)
      // Unsubscribe on destroy: a response landing after teardown would
      // otherwise allocate an object URL into an already-cleared cache (a leak)
      // and toast a "Render failed" into whatever view replaced this one.
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (blob) => {
          if (id !== this.reqId) return; // superseded — drop it, nothing allocated yet
          const blobUrl = URL.createObjectURL(blob);
          // Chrome/PDFium honours PDF open parameters on blob URLs: this fragment
          // hides the viewer's dark toolbar and thumbnail nav pane and fits the
          // page to width, so the preview reads as the *document* rather than a
          // browser PDF viewer. Append to the URL *string* before sanitizing so
          // the fragment survives into the SafeResourceUrl; keep the bare
          // `blobUrl` (no fragment) for `URL.revokeObjectURL`, which keys on it.
          const safe = this.sanitizer.bypassSecurityTrustResourceUrl(
            `${blobUrl}#toolbar=0&navpanes=0&view=FitH`,
          );
          this.cache.set(key, { blobUrl, safe });
          this.url.set(safe);
          this.rendering.set(false);
        },
        error: (err: unknown) => {
          if (id !== this.reqId) return;
          this.rendering.set(false);
          this.error.emit(err);
        },
      });
  }

  /** Revoke every cached object URL and empty the cache. */
  private clearCache(): void {
    for (const { blobUrl } of this.cache.values()) {
      URL.revokeObjectURL(blobUrl);
    }
    this.cache.clear();
  }
}
