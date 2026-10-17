# iPhone / iPad build

Papr shares its SQLite library, RSS ingestion, WebDAV replication and article AI code across desktop and iOS. The phone UI switches between the library, article list and reader, with touch navigation, safe-area padding, full-screen settings and a mobile AI drawer. The minimum deployment version is **iOS 15.0**, including **iOS 18.3**. A newer iPhoneOS SDK does not raise the deployment target.

The iOS app starts with its own sandbox and database. Connect the same WebDAV directory in **Settings → Sync → WebDAV** to merge subscriptions, articles, tags, reading state and saved AI preferences. Device credentials are not transferred. No personal database or credentials are included in the IPA.

AI summary, article chat and LLM translation support API providers. Desktop Codex sign-in requires a subprocess and is unavailable on iOS. If WebDAV imports a Codex selection, Papr preserves that preference and explains the limitation; choose an API provider and configure its key on the phone. Changing the shared provider/model is also synced to desktop. Originals open in the system browser. Desktop tray, auto-start and in-app binary updates are unavailable. Refresh and scheduled WebDAV transfers run while the app is active; returning online or to the foreground also triggers WebDAV sync. Reading progress is flushed when the app is hidden. iOS background execution is not guaranteed.

## Unsigned device IPA

Requirements: macOS, full Xcode with the iPhoneOS SDK, Rust's `aarch64-apple-ios` target, Node and project dependencies. Initialize the icons and Xcode project once using the local Tauri CLI:

```sh
node node_modules/@tauri-apps/cli/tauri.js ios init
python3 scripts/build-ios-unsigned.py
```

The script builds the frontend and the Tauri Rust/Swift static library, links a native arm64 application using the iPhoneOS SDK and packages `Payload/Papr.app`. It uses the iOS dictionary launch screen and PNG app icons; it does not need a simulator runtime or signing credentials. Output:

`target/ios-unsigned/Papr_0.16.1_iOS_unsigned.ipa`

The generated Xcode project under `src-tauri/gen/apple` remains available for standard Xcode development and a signed build. Tauri's normal `ios build --no-sign` route additionally requires the iOS platform/simulator runtime to be installed. This Mac has the SDK but has not completed that runtime installation.

The IPA is intentionally unsigned and has no provisioning profile. Sign it using your own Apple account and compatible signing tool before installing on an ordinary device. Preserve the same bundle identifier and signing setup for upgrades if you want to retain that installation's sandbox data. See [Tauri iOS code signing](https://v2.tauri.app/distribute/sign/ios/).

Verification covers arm64 Mach-O format, iPhoneOS platform and deployment version, system framework dependencies, IPA structure, absence of signatures and credentials, mobile browser layout and navigation, shared library tests and desktop build compatibility. Real-device testing is still needed after signing.

## iPhone layout corrections (build 2)

The app uses the Papr artwork from `public/papr.svg` for its iPhone and iPad icons, encoded as opaque RGB PNGs. The packaging script checks the required icon dimensions and device-specific declarations.

The reader keeps common touch actions visible and places secondary actions in a More menu. Settings use a section picker on phones; long titles and metadata wrap within the screen.

CSS applies notch and Home indicator padding once. The iOS WKWebView disables automatic scroll-view content insets, and the app shell follows the actual window height instead of relying on dynamic viewport units. Opening the keyboard temporarily reduces the shell height; dismissing it restores the full window. Browser checks cover 393×852, 320×568 and landscape layouts, with separate tests for safe-area and keyboard height calculations. These checks do not replace testing on an iPhone 14 Pro running iOS 18.3.

Build 3 disables desktop hover previews on iOS and ignores touch/pen pointer entry on desktop. iOS touch-generated mouse events previously opened an undismissed preview over the status bar. Scrolling, pressing or opening an article now clears pending previews; delayed desktop previews also reject detached or recycled virtual rows.

Build 4 gives menus, tag pickers and highlight controls a shared measured layout within the safe visible viewport. It handles async content growth, landscape cutouts, keyboard height and viewport panning; oversized content scrolls inside the available area. Search, confirmation/rename/add-subscription dialogs, settings, image-viewer controls and toast messages also respect the same safe-area variables. Floating controls and nested confirmation dialogs mount at the document body so a parent's animation or backdrop filter cannot distort their coordinates. Mobile tag and search popovers have explicit close buttons, and outside-touch dismissal uses pointer events.
