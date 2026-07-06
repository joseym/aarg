/** Trigger a browser download of a blob under a filename, via a transient
 *  object URL and a synthetic anchor click. Shared by the tailoring workspace
 *  (rendered PDFs) and the chat panel's artifact cards. */
export function triggerDownload(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}
