# Papr — Feed Finder (browser extension)

A dependency-free Manifest V3 extension that detects RSS / Atom / JSON feeds on
the page you are viewing and subscribes to them in the **Papr** desktop app
with one click.

## What it does

- Scans every page for `<link rel="alternate" type="application/rss+xml |
  atom+xml | json">` tags.
- Recognises well-known sources that do not declare a feed: YouTube channels
  and playlists, Reddit subreddits, and Mastodon profiles — the feed URL is
  derived the same way the Papr desktop app does it.
- Shows a badge with the feed count on the toolbar icon when the current page
  has feeds.
- The popup lists every detected feed with a **Subscribe in Papr** button.

## How "Subscribe in Papr" works

The button opens a custom-scheme deep link:

```
papr://subscribe?url=<url-encoded feed URL>
```

The Papr desktop app registers the `papr://` scheme. Opening the link focuses
the Papr window and opens its **Add feed** dialog prefilled with the feed URL.
Papr must be installed and have been launched at least once for the OS to know
about the scheme.

## Permissions

The extension requests **no permissions**. Feed detection runs entirely in a
declared content script (`content_scripts` in the manifest), and the popup
talks to that content script with `chrome.tabs.sendMessage` — none of which
needs the `"tabs"` permission. Reading a tab's URL or title *would* require it,
but the extension never does: it only ever uses `tab.id`. Keeping the
permission list empty means the browser shows no install-time warning.

## Install for development (load unpacked)

### Chrome / Edge / Brave

1. Open `chrome://extensions`.
2. Enable **Developer mode** (top-right toggle).
3. Click **Load unpacked** and select this `extension/` directory.
4. The Papr icon appears in the toolbar. Visit a blog or a YouTube channel and
   click it.

### Firefox

1. Open `about:debugging#/runtime/this-firefox`.
2. Click **Load Temporary Add-on…**.
3. Select the `extension/manifest.json` file.
4. The add-on stays loaded until Firefox is restarted.

## Project layout

```
extension/
  manifest.json        MV3 manifest (Chrome + Firefox)
  popup.html / popup.js  toolbar popup UI
  src/
    detect.js          pure feed-detection logic (tested under Node)
    content.js         reads the page DOM, reports to the background worker
    background.js      service worker — paints the toolbar badge
  icons/               toolbar icons
```

## Tests

`src/detect.js` is a pure, environment-agnostic module (no `import`/`export`,
no DOM access). It exposes its API both as a CommonJS module (for Node) and as
`globalThis.PaprDetect` (for the extension). Its unit tests live in the repo
root and run with vitest:

```
pnpm test            # from the repository root
```
