/** Trigger a browser download of a blob under a filename, via a transient
 *  object URL and a synthetic anchor click. Shared by the tailoring workspace
 *  (rendered PDFs) and the chat panel's artifact cards.
 *
 *  `a.click()` only queues the download; the browser reads from the object
 *  URL after this function returns, not during it. Revoking the URL in the
 *  same tick as the click raced that read on at least one browser (Arc),
 *  which fell back to treating the download as unconfirmed rather than
 *  reading a blob that might already be gone. A one-tick delay lets the
 *  download actually start before the URL is invalidated. */
export function triggerDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}
