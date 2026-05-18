// Trigger a browser download of in-memory content.
//
// No file-dialog plugin is bundled, so exports (OPML, highlight Markdown) are
// offered to the user as a webview download via a synthetic <a download>.
//
// Two details matter for this to work reliably inside Tauri's WKWebView (the
// macOS webview is WebKit, not Chromium):
//   1. The anchor must be connected to the document — WebKit ignores a click
//      on a detached element, so a never-appended anchor silently downloads
//      nothing.
//   2. The blob's object URL must outlive the click. Revoking it synchronously
//      right after `a.click()` can free the blob before the download has been
//      dispatched, yielding an empty or failed file. Defer the revoke.

/** Download `content` as a file named `filename` with the given MIME type. */
export function downloadFile(
  content: string,
  filename: string,
  mimeType: string,
): void {
  const blob = new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  // Let the download dispatch before tearing down the URL and the anchor.
  window.setTimeout(() => {
    a.remove();
    URL.revokeObjectURL(url);
  }, 0);
}
