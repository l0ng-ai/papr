export type ImageBytesResponse = ArrayBuffer | Uint8Array | number[];

export function imageBytes(data: ImageBytesResponse): Uint8Array {
  if (data instanceof Uint8Array) return data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  return new Uint8Array(data);
}

/** Detect an image's MIME type from its leading "magic" bytes, falling back to
 *  the URL's file extension, and finally to image/jpeg. The type matters for a
 *  data: URL: unlike a Blob (whose bytes the webview sniffs), a data: URL's
 *  MIME is *declared*, so a wrong or empty type makes the <img> fail to render.
 *  Many CDN image URLs carry no extension, which is why the byte sniff comes
 *  first. */
export function imageMime(src: string, bytes: Uint8Array): string {
  const sniffed = sniffImageMime(bytes);
  if (sniffed) return sniffed;
  const path = src.split(/[?#]/, 1)[0]?.toLowerCase() ?? "";
  if (path.endsWith(".png")) return "image/png";
  if (path.endsWith(".webp")) return "image/webp";
  if (path.endsWith(".gif")) return "image/gif";
  if (path.endsWith(".svg")) return "image/svg+xml";
  if (path.endsWith(".jpg") || path.endsWith(".jpeg")) return "image/jpeg";
  return "image/jpeg";
}

/** MIME type from the byte signature, or null when none of the known image
 *  formats match. SVG is text (no fixed magic bytes), so it is left to the
 *  extension fallback in `imageMime`. */
function sniffImageMime(b: Uint8Array): string | null {
  // PNG: 89 50 4E 47
  if (b.length >= 4 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47)
    return "image/png";
  // JPEG: FF D8 FF
  if (b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff)
    return "image/jpeg";
  // GIF: "GIF"
  if (b.length >= 3 && b[0] === 0x47 && b[1] === 0x49 && b[2] === 0x46)
    return "image/gif";
  // WebP: "RIFF"…"WEBP"
  if (
    b.length >= 12 &&
    b[0] === 0x52 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x46 &&
    b[8] === 0x57 && b[9] === 0x45 && b[10] === 0x42 && b[11] === 0x50
  )
    return "image/webp";
  return null;
}

/** Encode recovered image bytes as a self-contained data: URL.
 *
 *  Images that fail to load directly (hotlink-protected hosts) are refetched
 *  through the backend and re-injected here. A blob: URL would be the obvious
 *  carrier, but WKWebView/WebView2 silently drop a blob:'s backing data under
 *  memory pressure (e.g. layer recompositing while scrolling), so the recovered
 *  image vanishes the moment the user scrolls away and back. A data: URL keeps
 *  the bytes inline in the DOM, so the webview can always repaint it — and it
 *  needs no revoke bookkeeping. */
export function imageDataUrl(src: string, bytes: Uint8Array): string {
  return `data:${imageMime(src, bytes)};base64,${bytesToBase64(bytes)}`;
}

/** Base64-encode bytes in chunks. A one-shot `String.fromCharCode(...bytes)`
 *  spreads the whole array as arguments and overflows the call stack on large
 *  images; chunking keeps each spread small. */
function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}
