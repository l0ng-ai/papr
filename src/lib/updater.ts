// In-app auto-update (desktop only).
//
// `check()` hits the GitHub release feed declared in tauri.conf.json, verifies
// the bundle signature against the embedded public key, and reports whether a
// newer version exists. When the user accepts, `downloadAndInstall()` fetches
// and applies it, then `relaunch()` restarts into the new build.

import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import i18n from "../i18n";
import { isDesktop } from "./platform";
import { toast, useToasts, reportError } from "../toast";

// Guards against overlapping checks (startup auto-check racing a manual one).
let inFlight = false;

/**
 * Check for an update. `silent` suppresses the "you're up to date" and error
 * toasts — used for the unobtrusive check on launch; the manual button in
 * Settings passes `silent: false` so the user always gets feedback.
 */
export async function checkForUpdates({ silent }: { silent: boolean }): Promise<void> {
  // The updater and process plugins are desktop-only (not registered on iOS,
  // where the App Store handles updates), so `check()` / `relaunch()` would
  // throw. Bail before touching them — this covers any caller, including a
  // manual check from Settings.
  if (!isDesktop) return;
  if (inFlight) return;
  inFlight = true;
  try {
    const update = await check();
    if (update) {
      offerUpdate(update);
    } else if (!silent) {
      toast.show(i18n.t("update.upToDate"));
    }
  } catch (e) {
    // On launch a missing release feed / dev build just means "no update" —
    // stay quiet. A manual check surfaces the failure.
    if (!silent) reportError(e);
  } finally {
    inFlight = false;
  }
}

/** Raise a persistent toast offering to install `update` and relaunch. */
function offerUpdate(update: Update): void {
  useToasts.getState().push({
    text: i18n.t("update.available", { version: update.version }),
    tone: "default",
    // Hold until the user acts; an update prompt shouldn't vanish on its own.
    duration: 3_600_000,
    action: {
      label: i18n.t("update.install"),
      run: () => void installAndRelaunch(update),
    },
  });
}

async function installAndRelaunch(update: Update): Promise<void> {
  try {
    toast.show(i18n.t("update.installing"));
    await update.downloadAndInstall();
    await relaunch();
  } catch (e) {
    reportError(e);
  }
}
