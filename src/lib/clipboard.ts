/** Keep copying inside the user's click, with an older WebView fallback. */
export async function copyText(text: string): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) { await navigator.clipboard.writeText(text); return; }
  } catch { /* Try the selection-based WebView API below. */ }
  const focused = document.activeElement as HTMLElement | null;
  const field = document.createElement("textarea");
  field.value = text; field.readOnly = true;
  field.style.cssText = "position:fixed;top:0;left:0;opacity:0;pointer-events:none;font-size:16px";
  document.body.appendChild(field);
  try { field.focus(); field.select(); field.setSelectionRange(0, text.length); if (!document.execCommand("copy")) throw new Error("Copy failed"); }
  finally { field.remove(); focused?.focus({ preventScroll: true }); }
}
