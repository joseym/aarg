import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  input,
  signal,
} from '@angular/core';

import { ApiService } from '../../services/api.service';
import { ChatStore } from '../../services/chat-store';
import {
  type ArtifactKind,
  type CoverPayload,
  coverToText,
  resumeToText,
} from './artifacts';

/** The fixed title and kind label per artifact. */
const META: Record<ArtifactKind, { title: string; label: string }> = {
  job_description: { title: 'Original posting', label: 'Job description' },
  resume: { title: 'Tailored resume', label: 'Resume' },
  cover_letter: { title: 'Cover letter', label: 'Cover letter' },
};

/** The stored PDF filenames a card downloads, matching the render output. */
const ATS_PDF = 'resume.ats.pdf';
const HUMAN_PDF = 'resume.human.pdf';
const COVER_PDF = 'cover_letter.pdf';

/** One download button a card offers: a label and the stored filename to fetch
 *  from the build's files route. */
interface Download {
  label: string;
  file: string;
}

/** An artifact card in the chat transcript: a titled, collapsed-by-default blob
 *  showing one of the build's saved documents, with expand, copy, and download.
 *
 *  Every card's content is RETRIEVED from the open build, never generated: the
 *  posting text and the resume come from the in-memory chat context (`jd.raw_text`
 *  and the canonical draft), the cover letter from its stored payload fetched by
 *  build id. The model only ever names which artifact to show; it never supplies
 *  the bytes. When a document does not exist for this build the card says so
 *  plainly rather than inventing one. */
@Component({
  selector: 'app-artifact-card',
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="art-card" [class.open]="expanded()">
      <button
        class="art-head"
        type="button"
        [attr.aria-expanded]="expanded()"
        (click)="toggle()"
      >
        <span class="art-chev" aria-hidden="true">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M9 6l6 6-6 6" />
          </svg>
        </span>
        <span class="art-titles">
          <span class="art-kind">{{ meta().label }}</span>
          <span class="art-title">{{ meta().title }}</span>
        </span>
      </button>

      <div class="art-actions">
        @if (bodyText()) {
          <button class="art-btn" type="button" (click)="copy()">
            {{ copied() ? 'Copied' : 'Copy' }}
          </button>
        }
        @for (dl of downloads(); track dl.file) {
          <button
            class="art-btn"
            type="button"
            [disabled]="downloading() === dl.file"
            (click)="download(dl.file)"
          >
            {{ downloading() === dl.file ? 'Downloading…' : dl.label }}
          </button>
        }
      </div>

      @if (expanded()) {
        <div class="art-body">
          @if (bodyText()) {
            <pre class="art-text">{{ bodyText() }}</pre>
          } @else {
            <p class="art-empty">{{ emptyMessage() }}</p>
          }
          @if (sourceUrl()) {
            <a class="art-source" [href]="sourceUrl()" target="_blank" rel="noopener noreferrer">
              Open the original posting URL
            </a>
          }
          @if (downloadError()) {
            <p class="art-err">{{ downloadError() }}</p>
          }
        </div>
      }
    </div>
  `,
  styles: `
    :host { display: block; }

    .art-card {
      border: 1px solid var(--border);
      border-radius: 12px;
      background: color-mix(in oklch, var(--surface) 88%, var(--surface-2));
      overflow: hidden;
    }
    .art-card.open { background: var(--surface); }

    .art-head {
      display: flex; align-items: center; gap: 9px; width: 100%;
      padding: 10px 12px; border: none; background: none; cursor: pointer;
      color: inherit; text-align: left;
    }
    .art-head:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
    .art-chev {
      flex-shrink: 0; display: inline-flex; color: var(--muted);
      transition: transform 0.16s ease;
    }
    .art-card.open .art-chev { transform: rotate(90deg); }
    .art-chev svg { width: 15px; height: 15px; }
    .art-titles { display: flex; flex-direction: column; gap: 1px; min-width: 0; }
    .art-kind {
      font-family: var(--font-mono); font-size: 9.5px; letter-spacing: 0.11em;
      text-transform: uppercase; color: var(--accent);
    }
    .art-title {
      font-family: var(--font-display); font-size: 14px; line-height: 1.2; color: var(--fg);
      overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
    }

    .art-actions {
      display: flex; flex-wrap: wrap; gap: 6px;
      padding: 0 12px 10px 36px;
    }
    .art-btn {
      padding: 4px 10px; border-radius: 8px; border: 1px solid var(--border);
      background: var(--surface); color: var(--fg); cursor: pointer;
      font-family: var(--font-mono); font-size: 10.5px; letter-spacing: 0.04em;
      transition: border-color 0.14s, color 0.14s;
    }
    .art-btn:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
    .art-btn:disabled { opacity: 0.55; cursor: default; }
    .art-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }

    .art-body {
      padding: 0 12px 12px 36px;
      display: flex; flex-direction: column; gap: 8px;
    }
    .art-text {
      margin: 0; max-height: 340px; overflow: auto;
      padding: 10px 12px; border-radius: 9px;
      border: 1px solid var(--border); background: var(--surface-2);
      font-family: var(--font-mono); font-size: 12px; line-height: 1.5;
      color: var(--fg); white-space: pre-wrap; word-break: break-word;
    }
    .art-empty { margin: 0; font-size: 12.5px; line-height: 1.5; color: var(--muted); }
    .art-source { font-size: 12px; color: var(--accent); }
    .art-err {
      margin: 0; font-size: 12px; line-height: 1.45;
      color: color-mix(in oklch, var(--danger) 80%, var(--fg));
    }
  `,
})
export class ArtifactCard implements OnInit {
  private readonly store = inject(ChatStore);
  private readonly api = inject(ApiService);

  /** Which document this card shows. */
  readonly artifact = input.required<ArtifactKind>();

  protected readonly expanded = signal(false);
  protected readonly copied = signal(false);
  /** The stored filename currently being fetched, or null when idle. */
  protected readonly downloading = signal<string | null>(null);
  protected readonly downloadError = signal<string | null>(null);
  /** The cover-letter text, fetched from `cover_payload.json` on init. Null until
   *  loaded, when there is no cover for this build, or when the fetch failed. */
  private readonly coverText = signal<string | null>(null);

  protected readonly meta = computed(() => META[this.artifact()]);

  /** The build's chat context. Present while the card is in the transcript (a
   *  build switch clears the transcript and its cards together). */
  private readonly ctx = computed(() => this.store.context());

  /** The copyable body text, or null when the document does not exist for this
   *  build (the card then shows its honest empty state instead). */
  protected readonly bodyText = computed<string | null>(() => {
    const ctx = this.ctx();
    if (!ctx) return null;
    switch (this.artifact()) {
      case 'job_description': {
        const raw = ctx.jd.raw_text?.trim();
        return raw && raw.length > 0 ? raw : null;
      }
      case 'resume':
        return ctx.canonical ? resumeToText(ctx.canonical) : null;
      case 'cover_letter':
        return this.coverText();
    }
  });

  /** The honest note shown when the document is not available for this build. */
  protected readonly emptyMessage = computed<string>(() => {
    switch (this.artifact()) {
      case 'job_description':
        return "The original posting text wasn't saved for this build.";
      case 'resume':
        return 'This build has no tailored resume yet.';
      case 'cover_letter':
        return this.hasCoverPdf()
          ? "The cover letter's text isn't available, but you can download the PDF."
          : 'This build has no cover letter.';
    }
  });

  /** The posting's source URL, shown as a link on the JD card when present. */
  protected readonly sourceUrl = computed<string | null>(() => {
    if (this.artifact() !== 'job_description') return null;
    return this.ctx()?.jd.source_url ?? null;
  });

  /** The download buttons for this artifact, each present only when its stored
   *  file exists for the build. */
  protected readonly downloads = computed<Download[]>(() => {
    const ctx = this.ctx();
    if (!ctx) return [];
    switch (this.artifact()) {
      case 'job_description':
        // The JD text downloads client-side (no stored .txt file); handled in
        // `download` by the synthetic name below.
        return this.bodyText() ? [{ label: 'Download .txt', file: JD_TXT }] : [];
      case 'resume': {
        const out: Download[] = [];
        if (ctx.pdfs.includes(ATS_PDF)) out.push({ label: 'ATS PDF', file: ATS_PDF });
        if (ctx.pdfs.includes(HUMAN_PDF)) out.push({ label: 'Human PDF', file: HUMAN_PDF });
        return out;
      }
      case 'cover_letter':
        return this.hasCoverPdf() ? [{ label: 'Download PDF', file: COVER_PDF }] : [];
    }
  });

  private hasCoverPdf(): boolean {
    return this.ctx()?.pdfs.includes(COVER_PDF) ?? false;
  }

  ngOnInit(): void {
    // The cover letter's text lives in a stored payload, not the in-memory
    // context, so fetch it once when the card is for a build that has a cover.
    if (this.artifact() === 'cover_letter' && this.hasCoverPdf()) {
      this.loadCoverText();
    }
  }

  private loadCoverText(): void {
    const ctx = this.ctx();
    if (!ctx) return;
    this.api.getBuildFile(ctx.buildId, 'cover_payload.json').subscribe({
      next: async (blob) => {
        try {
          const payload = JSON.parse(await blob.text()) as CoverPayload;
          const text = coverToText(payload);
          this.coverText.set(text.length > 0 ? text : null);
        } catch {
          // A present-but-unreadable payload: leave the text unavailable; the PDF
          // download still works and the empty state explains it.
          this.coverText.set(null);
        }
      },
      // No payload on disk (older build): text stays unavailable, PDF still fine.
      error: () => this.coverText.set(null),
    });
  }

  protected toggle(): void {
    this.expanded.update((v) => !v);
  }

  protected async copy(): Promise<void> {
    const text = this.bodyText();
    if (!text) return;
    await writeClipboard(text);
    this.copied.set(true);
    setTimeout(() => this.copied.set(false), 1500);
  }

  protected download(file: string): void {
    this.downloadError.set(null);
    // The JD text has no stored file; it downloads from the in-memory raw text.
    if (file === JD_TXT) {
      const text = this.bodyText();
      if (text) triggerDownload(new Blob([text], { type: 'text/plain' }), 'job-posting.txt');
      return;
    }
    const ctx = this.ctx();
    if (!ctx) return;
    this.downloading.set(file);
    this.api.getBuildFile(ctx.buildId, file).subscribe({
      next: (blob) => {
        triggerDownload(blob, file);
        this.downloading.set(null);
      },
      error: () => {
        this.downloading.set(null);
        this.downloadError.set(`Couldn't download ${file}.`);
      },
    });
  }
}

/** Synthetic filename for the JD's client-side .txt download (no stored file). */
const JD_TXT = '__jd_txt__';

/** Trigger a browser download of a blob under a filename. */
function triggerDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** Copy text to the clipboard, falling back to a hidden textarea when the async
 *  Clipboard API is unavailable (older engine, or a non-secure context). */
async function writeClipboard(text: string): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return;
    }
  } catch {
    // Fall through to the textarea path.
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  try {
    document.execCommand('copy');
  } finally {
    ta.remove();
  }
}
