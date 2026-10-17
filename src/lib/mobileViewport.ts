/** Use the native layout height until an on-screen keyboard actually opens.
 * WKWebView can report a smaller visual viewport just for safe-area insets;
 * treating that as the application height would reserve those insets twice. */
export function mobileViewportHeight(layout: number, visual: number | undefined, editing: boolean, scale = 1): number {
  if (editing && scale <= 1.01 && visual != null && visual > 0 && layout - visual > 120) return visual;
  return layout;
}

export function installMobileViewport(win: Window): () => void {
  const root = win.document.documentElement;
  let frame = 0;
  const update = () => {
    const editing = /^(INPUT|TEXTAREA)$/.test(win.document.activeElement?.tagName || "") ||
      (win.document.activeElement instanceof HTMLElement && win.document.activeElement.isContentEditable);
    const viewport = win.visualViewport;
    const height = mobileViewportHeight(win.innerHeight, viewport?.height, editing, viewport?.scale);
    root.style.setProperty("--app-height", `${Math.round(height)}px`);
    root.dataset.keyboardOpen = String(height < win.innerHeight - 120);
    root.style.setProperty("--app-top", `${root.dataset.keyboardOpen === "true" ? viewport?.offsetTop ?? 0 : 0}px`);
    win.dispatchEvent(new Event("papr-viewport-change"));
  };
  const schedule = () => { win.cancelAnimationFrame(frame); frame = win.requestAnimationFrame(update); };
  update();
  win.addEventListener("resize", schedule);
  win.visualViewport?.addEventListener("resize", schedule);
  win.visualViewport?.addEventListener("scroll", schedule);
  win.document.addEventListener("focusin", schedule);
  win.document.addEventListener("focusout", schedule);
  return () => {
    win.cancelAnimationFrame(frame);
    win.removeEventListener("resize", schedule);
    win.visualViewport?.removeEventListener("resize", schedule);
    win.visualViewport?.removeEventListener("scroll", schedule);
    win.document.removeEventListener("focusin", schedule);
    win.document.removeEventListener("focusout", schedule);
  };
}
