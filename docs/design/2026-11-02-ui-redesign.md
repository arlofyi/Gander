# UI redesign: design system and screen inventory

- Status: Proposed, for owner review before any code changes
- Date: 2026-11-02
- Scope: the Android client (phone and tablet) and the Windows host's tray and settings app
- Mockups: [mockups/index.html](mockups/index.html) (open it in a browser; 48 clickable screens,
  light and dark, sidebar index, arrow keys step through)
- Related: [Android client spec](../specs/2026-11-01-android-client-v1.md),
  [pairing and trust](../specs/2026-11-01-pairing-and-trust.md),
  [stream launch policy](../specs/2026-11-01-stream-launch-policy.md),
  [security](../security.md), [vision](../vision.md)

This document is the design system and the screen inventory. It changes no behaviour: every flow,
string and rule in the specs above still holds. Where the redesign proposes something new (a
layout, a component, a wording tweak), it is called out and listed under open questions.

## 1. Direction in one paragraph

Gander should feel like a careful instrument, not a remote-control app: quiet goose-down neutrals,
slate ink for text and commands, and **one** accent, greylag-beak orange, used only when something
is *looking* (a live session, the selected thing, the suggested default). Each platform is native:
Material 3 structure and Roboto on Android; Windows 11 Fluent structure, Segoe UI Variable and
compact density on the laptop. Two signature ideas carry the product's promise: the **work rail**,
a tiny two-device glyph on every mode that shows which device does the work, and **"orange means
looking"**, which makes the host's being-viewed state unmistakable because orange is otherwise rare.

## 2. Principles

1. **Say where the work happens.** Every Preview / Fetch / Stream choice shows its cost and the
   device that pays it (work rail plus a one-line hint), before the user commits.
2. **Orange is attention, not decoration.** It marks live sessions, the default action, selection
   and focus. Commands are ink; destructive actions are red. If a screen has no live or selected
   state, it has almost no orange.
3. **Honest before busy.** Unavailable things stay visible with the reason next to them
   ("studio-pc is locked"), in place, never as a toast after a spinner.
4. **Trust is legible.** Pairing shows both identities and a large grouped code; the host shows who
   is connected, how (direct or relayed) and what they are doing, at all times.
5. **Native, then branded.** Platform conventions win over brand flourishes: M3 components on
   Android, Fluent patterns on Windows. The brand lives in colour, the Lens mark and copy.
6. **Calm motion, no theatre.** Nothing pulses for attention except progress; the live dot is a
   static dot with a halo.

Clichés avoided on purpose: no gradients on surfaces (one faint desktop wallpaper glow in the
mockup frame only), no cream-and-terracotta (the paper colour moved to a neutral grey, see 3.1),
no emoji, and not every group is a rounded card: lists are full-bleed rows, the file sheet uses
stacked action panels, and host settings use Fluent setting rows.

## 3. Colour

### 3.1 Brand colours

| Name | Hex | Use |
| --- | --- | --- |
| Slate ink | `#1d2a30` | Text, primary commands, the Lens mark on light |
| Greylag beak | `#e2682a` | The single accent; the beak in the Lens mark is always this value |
| Paper | `#f4f2ec` | Kept for the mark's background, store art and the website, not for app surfaces |

Refinement: the starting paper `#f4f2ec` with orange reads as cream-and-terracotta in large areas,
so app surfaces use **goose-down** greys (`#f3f4f2`, `#fbfbfa`) that are a hair warm but not cream.

### 3.2 Tokens

Same token names in both themes; only the values change. Contrast ratios are WCAG 2.x, measured
against the surface they are used on (computed, not estimated).

| Token | Light | Dark | Use | Contrast notes |
| --- | --- | --- | --- | --- |
| `bg` | `#f3f4f2` | `#111719` | Screen background | |
| `surface` | `#fbfbfa` | `#172023` | Sheets, dialogs, raised panels | |
| `surface-2` | `#eceeec` | `#1e282c` | Containers, nav bar, action panels | |
| `surface-3` | `#e3e7e5` | `#263236` | Tiles, filled chips, tonal buttons | |
| `line` | `#d6dbda` | `#33424a` | 1 px dividers | decorative, no minimum |
| `outline` | `#7e8b91` | `#6f7d83` | Control edges (fields, chips, radios) | 3.39:1 on surface / 3.90:1 dark: meets 3:1 for UI |
| `text` | `#1d2a30` | `#e7ecec` | Body and titles | 13.4:1 on bg / 15.2:1 |
| `text-2` | `#46555c` | `#b3c0c4` | Secondary text | 7.0:1 / 8.9:1 |
| `text-3` | `#5f6e75` | `#8d9ba0` | Muted text, metadata | 4.79:1 on bg, 4.53:1 on surface-2 / 5.8:1: the floor for text |
| `primary` | `#1d2a30` | `#e7ecec` | Commands (filled buttons, FAB, switches on) | on-primary 14.7:1 / 12.4:1 |
| `on-primary` | `#ffffff` | `#1d2a30` | | |
| `accent` | `#e2682a` | `#ee7a3c` | Live dot, default outline, selection, focus ring | 3.25:1 on surface (light): **non-text only** or large text; 5.4:1+ on dark |
| `on-accent` | `#170d06` | `#170d06` | Text on an orange fill (live band, Default tag, sticky key) | 5.7:1 on `#e2682a` |
| `accent-text` | `#a8460f` | `#f59a62` | Orange-coloured text (LIVE label, links) | 5.7:1 on surface / 7.6:1 |
| `accent-container` | `#fbe3d4` | `#4a2412` | Selected rows, default action panel, nav indicator | on-accent-container 10.2:1 / 10.5:1 |
| `on-accent-container` | `#5a2206` | `#ffdccb` | | |
| `ok` | `#2e7d4f` | `#6fcf97` | Online, verified, identity checked | 4.87:1 on surface / 8.7:1 |
| `warn` | `#8a6100` | `#e3b341` | Relayed, metered, caution | 5.35:1 / 8.5:1 |
| `bad` | `#b3261e` | `#f2b8b5` | Refused, mismatch, error text | 6.3:1 / 9.7:1 |
| `danger` | `#b3261e` | `#c4302b` | Fill of destructive buttons (kill switch) | white text 6.5:1 / 5.5:1 |
| `ok-c`, `warn-c`, `bad-c` | `#dcefe2`, `#f6e7c4`, `#f9dedc` | `#1f3a2a`, `#3d3012`, `#4a1d1a` | Banner backgrounds; text on them uses `text` | |
| `scrim` | ink at 42 % | black at 55 % | Behind sheets and dialogs | |

Windows host additions (Mica stand-ins, see section 9):

| Token | Light | Dark | Use |
| --- | --- | --- | --- |
| `mica` | `#eef0ef` | `#1b2124` | Window background, nav pane, title bar |
| `layer` | `#fbfbfa` | `#232b2e` | Cards and setting rows |
| `layer-2` | `#f6f7f6` | `#1f2629` | The content page behind cards |
| `stroke` | ink at 10 % | white at 7 % | Card and layer borders |
| `stroke-2` | ink at 16 % | white at 13 % | Control borders, window border |

### 3.3 How orange is used (the whole list)

- Live: the LIVE dot and label on the phone, the live band across the host window, the
  top-of-screen pill, the orange dot on the tray icon, a "Being viewed" status line.
- Default: the outline and "Default" tag of the suggested action in the file sheet.
- Selection and focus: the nav indicator, selected rows and chips, tabs underline, radio dots, the
  QR reticle, a latched sticky modifier key, keyboard focus rings.
- The beak of the Lens mark.

Never: large backgrounds, primary buttons, headings, illustrations, success states.

### 3.4 Status is never colour alone

Every status pairs colour with a shape or word: a filled dot plus "Online", a hollow ring plus
"Offline", the relay glyph plus "Relayed", an icon plus a sentence in banners, a crossed-out chip
for a mode the share does not allow.

## 4. Type

### 4.1 Android (Material 3 roles)

Font: the system sans (Roboto on most devices; the mockup uses Roboto Flex as a stand-in) and
`FontFamily.Monospace` for paths, text preview and the pairing code. Nothing bundled in v1.

| Role | Size / line | Weight | Used for |
| --- | --- | --- | --- |
| `headlineMedium` | 28 / 36 sp | 400 | Large screen titles ("Your computers") |
| `titleLarge` | 22 / 28 | 500 | Top app bar, dialog titles |
| `titleMedium` | 16 / 24 | 600 | Host names, action titles in the file sheet |
| `bodyLarge` | 16 / 24 | 400 | List row primary text, fields |
| `bodyMedium` | 14 / 20 | 400 | Body, hints |
| `bodySmall` | 12 / 16 | 400 | Metadata, captions |
| `labelLarge` | 14 / 20 | 500 | Buttons |
| `labelMedium` | 12 / 16 | 500 | Chips, tags, key caps |
| `sas` (custom) | 52 / 60 | 500, mono, +4 sp tracking | Pairing code, grouped 3 + 3 in two boxes |

### 4.2 Windows host (Fluent ramp, compact)

Font: Segoe UI Variable (Text for body, Display for titles), loaded from the system at run time;
Cascadia Mono (ships with Windows 11) for addresses, paths and the pairing code.

| Role | Size / line | Weight | Used for |
| --- | --- | --- | --- |
| Title | 26 / 34 px | Semibold | Page titles |
| Subtitle | 20 / 28 | Semibold | Dialog titles, pairing request title |
| Body strong | 14 / 20 | Semibold | Section labels, device names |
| Body | 13 / 18 | Regular | Everything else (compact density) |
| Caption | 12 / 16 | Regular | Descriptions under setting titles, table headers |
| Code | 52 / 56 | Semibold mono | Pairing code on the approve screen |

## 5. Space, shape, elevation

- **Grid:** 4 dp / 4 px. Android screen gutters 16-20 dp; host page padding 20 x 26 px.
- **Touch targets:** 48 dp minimum on Android (icon buttons are 44 dp drawn inside a 48 dp target);
  stream keys 38 dp tall, 6 per row. Host controls 30 px tall, nav items 36 px.
- **Radius, Android:** chips 8, small tags 5, tiles 10-14, action panels 16, cards 20, sheets and
  dialogs 28, buttons fully rounded.
- **Radius, Windows:** controls 4, cards and setting rows 6, windows, flyouts and dialogs 8.
- **Elevation, Android:** tonal (surface, surface-2, surface-3), no shadows except the FAB and the
  stream overlays.
- **Elevation, Windows:** mica, then layer with a 1 px stroke. Shadows only on flyouts, dialogs,
  toasts and the top-of-screen pill.

## 6. Iconography

- 24 px grid, 1.8 px outlined strokes, round caps; filled only for play, the status dots and the
  Lens mark. In code: Material Symbols Outlined on Android (Apache-2.0); on Windows either the same
  set rendered into egui or Segoe Fluent Icons from the system (open question 6).
- **Mode icons:** Preview = eye, Fetch = arrow down into a tray, Stream = screen with a play
  triangle. Used identically on both platforms and in the activity log.
- **The work rail** (signature): computer, link, phone; the device that does the work is filled
  orange. Preview shows a page travelling to the phone, Fetch shows bytes and an arrow into the
  filled phone, Stream shows a dashed mirror link and a window outline on the phone. Always paired
  with words ("Work on studio-pc", "Work on this phone", "Runs on studio-pc"), so it is never the
  only carrier of meaning.
- **The Lens mark:** in the app bar of the tablet rail, the lock screen, the empty state, the tray
  and the host title bar. The eye dot takes the background colour, the beak stays `#e2682a` in both
  themes. Tray icon variants: idle (ink or white per taskbar theme), live (adds an orange dot),
  paused (40 % opacity).

## 7. Motion

- Durations: 100 ms for state changes (chips, toggles), 200 ms for sheets and dialogs (M3
  emphasized decelerate in, accelerate out), 250 ms for screen transitions (M3 shared axis X for
  hierarchy, fade-through for tabs). Windows: Fluent's 167 ms fade and 250 ms slide for flyouts.
- Progress is the only looping animation (indeterminate bars). The live dot does not pulse.
- The pairing code never animates or re-flows once shown.
- Honour reduced motion (Android "Remove animations", Windows "Animation effects" off): cross-fades
  become instant, indeterminate bars become a static partial bar plus text.

## 8. Components

| Component | Android | Windows host | Notes |
| --- | --- | --- | --- |
| Host block | list row with 48 dp tile, status line, relay chip, share shortcuts | -- | Not a card; rows split by a hairline |
| Status line | dot + word | dot + word | `ok`, hollow ring for offline, `warn` for relayed, orange for live |
| Relay chip | outlined `warn` chip with relay glyph | inline `warn` text | Always followed by one plain sentence |
| Permission chips | filled = allowed, crossed out = not allowed | checkbox grid per device | Stream column tinted with `accent-container` when on |
| Breadcrumb | scrollable crumb row | -- | Current segment bold |
| File row / grid card | type tile (neutral) + name + meta + flags | -- | Saved-on-phone badge is a small green check on the tile |
| Action panel | M3 surface-2 panel with icon, title, hint, reason line, work rail | -- | Default: accent-container fill + 2 dp accent outline + "Default" tag; unavailable: outline only, 50 % content, reason at full contrast |
| Reason line | icon + sentence, full contrast | infobar | Reasons stay readable even when the action is disabled |
| Banner | tinted container with a 3 dp left rule in the status colour | Fluent InfoBar | ok / warn / bad / neutral / live |
| Live band | -- | full-width orange band under the title bar with Stop | Only while a stream is on |
| Top-of-screen pill | -- | always-on-top orange pill, device, app, time, Stop | Separate borderless window, see 9.4 |
| Toast | -- | Windows notification layout | Title names device and file; one danger action when relevant |
| Pairing identity pair | two identity tiles joined by a dashed link | device card with identity line | Both names always visible next to the code |
| Pairing code | two boxed groups of 3 digits, mono 52 | mono 52, no boxes | TalkBack/Narrator read digit by digit |
| Session bar | close, LIVE + timer, where, Direct / Trackpad segmented, zoom reset | -- | Always visible during a stream |
| Keyboard bar | three rows of keys, sticky modifiers latch orange | -- | Tablet: one row |
| Setting row | M3 list item with supporting text | Fluent settings card (icon, title, description, control) | |
| Kill switch | -- | outlined danger button in the nav, filled danger in the tray flyout | One click, no confirmation; undo is "Resume now" |
| Dialogs and sheets | M3 basic dialog, modal bottom sheet, side detail pane on tablets | ContentDialog with footer | |

## 9. Mapping onto code

### 9.1 Compose (`ui/theme/Theme.kt`)

Dynamic colour is turned **off** by default (the accent rules depend on a fixed palette; open
question 1). Light and dark `ColorScheme` values:

| M3 role | Gander token | Why |
| --- | --- | --- |
| `primary` / `onPrimary` | `primary` / `on-primary` | Buttons, FAB, switch track are ink |
| `primaryContainer` / `onPrimaryContainer` | `surface-3` / `text` | Tonal buttons stay neutral |
| `secondary` / `onSecondary` | `accent-text` / `surface` | Rarely used by components; kept on-brand |
| `secondaryContainer` / `onSecondaryContainer` | `accent-container` / `on-accent-container` | M3 uses it for the nav bar indicator, selected filter chips and segmented buttons |
| `tertiary` / `onTertiary` | `accent` / `on-accent` | The raw accent for the live dot, Default tag, sticky keys |
| `tertiaryContainer` / `onTertiaryContainer` | `accent-container` / `on-accent-container` | Default action panel |
| `background`, `surface` | `bg`, `bg` | |
| `surfaceContainerLowest` / `Low` / default / `High` / `Highest` | `surface` / `surface` / `surface-2` / `surface-3` / `surface-3` | Sheets, nav bar, tiles |
| `onSurface` / `onSurfaceVariant` | `text` / `text-2` | |
| `outline` / `outlineVariant` | `outline` / `line` | |
| `error` / `onError` / `errorContainer` / `onErrorContainer` | `bad` / `surface` / `bad-c` / `text` | |
| `scrim` | `scrim` | |
| `inverseSurface` / `inverseOnSurface` / `inversePrimary` | `text` / `bg` / `accent` | Snackbars |

Not in `ColorScheme`: `ok`, `warn`, `ok-c`, `warn-c`, `danger`, `accent-text`. They go in a small
`@Immutable data class GanderColors` provided by a `staticCompositionLocalOf` next to
`MaterialTheme`, read as `GanderTheme.colors.warn`. Component overrides: `RadioButton`, `Checkbox`
selection and `TabRow` indicator use `tertiary` (orange); `NavigationBar` and `NavigationRail`
indicators use `secondaryContainer`. `Typography` follows 4.1 with a `GanderTypography.sas` extra;
`Shapes`: extraSmall 6, small 8, medium 12, large 16, extraLarge 28.

Tablet layouts: `WindowSizeClass` expanded width switches the browser to two panes (list-detail via
`ListDetailPaneScaffold` from `material3-adaptive`, or a hand-rolled `Row` if the dependency is
unwanted) and the nav bar to a `NavigationRail`.

### 9.2 egui (`host/gander-host/src/ui/theme.rs`)

egui 0.33 `Visuals` fields, per theme:

| Field | Value |
| --- | --- |
| `dark_mode` | follows the system, overridable in Settings |
| `panel_fill` | `mica` (nav pane and title area) |
| `window_fill` | `layer` |
| `faint_bg_color` | `layer-2` (the content page behind cards; table striping off) |
| `extreme_bg_color`, `text_edit_bg_color` | `layer` (fields), with `stroke-2` borders |
| `code_bg_color` | `layer-2` |
| `override_text_color` | `None`; `widgets.*.fg_stroke.color` carry `text`, `weak_text_color` = `text-2` |
| `widgets.noninteractive` | `bg_fill` `layer`, `bg_stroke` 1 px `stroke`, `fg_stroke` `text`, radius 6 |
| `widgets.inactive` | `weak_bg_fill` `layer`, `bg_stroke` 1 px `stroke-2`, radius 4 |
| `widgets.hovered` | `weak_bg_fill` = `layer` mixed 4 % toward `text`, `expansion` 0 |
| `widgets.active` | `weak_bg_fill` = `layer` mixed 8 % toward `text` |
| `widgets.open` | as hovered |
| `selection.bg_fill` / `selection.stroke` | `accent-container` / 1 px `accent` (focus and text selection) |
| `hyperlink_color` | `accent-text` |
| `warn_fg_color` / `error_fg_color` | `warn` / `bad` |
| `window_stroke` | 1 px `stroke-2` |
| `window_corner_radius` / `menu_corner_radius` | 8 / 8 |
| `window_shadow` / `popup_shadow` | offset (0, 8), blur 24, black at 28 % (egui shadows are soft rects, fine for this) |
| `button_frame` | true; primary buttons drawn with `primary` fill via a helper, danger with `danger` |
| `striped` | false |

`Spacing`: `item_spacing` (8, 6), `button_padding` (12, 5), `interact_size.y` 30,
`menu_margin` 4, nav item height 36. The existing `theme::Tone` maps to `ok`, `warn`, `bad` and a
neutral info (ink, not blue), and `banner()` becomes the Fluent InfoBar: tinted `*-c` fill, 1 px
`stroke`, icon in the tone colour, text in `text`.

**Mica without blur.** egui cannot blur what is behind the window, so Mica is approximated with
two flat layers: the window background and nav pane in `mica`, the content page in `layer-2` with a
1 px `stroke` on its top and left edges and an 8 px top-left radius (exactly Fluent's
NavigationView look), and cards in `layer`. The values are tinted a few percent toward ink so the
app reads as Gander rather than default grey. (Real Mica is possible later through
`DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE)` with a transparent eframe viewport; not
worth the risk in v1.)

**Fonts.** Keep the bundled egui fonts as the fallback and, on Windows, add
`C:/Windows/Fonts/SegUIVar.ttf` (Segoe UI Variable) and `CascadiaMono.ttf` at start-up if present
(read from the system, never redistributed). egui renders variable fonts at their default
instance, so semibold uses `seguisb.ttf` as a separate family when available.

### 9.3 Live state on the host

- Tray icon swaps to the live variant; tooltip text already exists in `tray_state.rs`.
- Every Gander window shows the live band (`TopBottomPanel` under the title, orange fill,
  `on-accent` text, Stop button).
- The top-of-screen pill is its own viewport: `ViewportBuilder::with_decorations(false)`,
  `with_always_on_top()`, `with_transparent(true)`, `with_taskbar(false)`, positioned top centre of
  the monitor that shows the streamed window. It must not be capturable into the stream (WGC
  captures one HWND, so it is not), and it must stay out of the way of the app's title bar
  (10 px from the top edge, 36 px tall).
- The yellow Windows capture border stays on (stream launch policy); the mockup labels it.

### 9.4 Copy

The existing strings in `client/android/app/src/main/res/values/strings*.xml` and the host pages are
reused nearly verbatim; they already follow the honesty rules. New strings proposed by the mockups:
"Work on studio-pc" / "Work on this phone" / "Runs on studio-pc" (rail captions), "Identity
checked: this is the computer the QR code came from", "Screenshots are blocked on this screen",
"Pixel 9 is viewing PowerPoint", "Disconnect everyone" (replaces "Kill switch" as the visible
label; the feature keeps its name in docs), "Everyone was disconnected at 14:07", "Share a folder"
(replaces "Add folder…").

## 10. Screen inventory

States listed are the ones the screen must be able to show. "Offline" means the computer cannot
be reached; "relay" means a relayed Tailscale path; "locked" means the host session is locked.
Numbers match the mockup index.

### 10.1 Android phone

| # | Screen | Purpose | States |
| --- | --- | --- | --- |
| 02-03 | Computers | Home: each paired computer with state, path, "paired as" and share shortcuts | empty (first run); loading ("Checking…"); online direct; relay (chip + sentence); offline (last seen, what is cached); needs attention (revoked, identity mismatch) with the fix; private network not detected |
| 04 | Add computer: start | Choose QR or address; anti-scam warning first | default; camera unavailable |
| 05 | Add computer: scan | Camera, reticle, screenshots blocked | scanning; not a Gander code / damaged / other version; camera permission needed |
| 06 | Add computer: address | Manual entry (flow B) | empty; invalid; connecting |
| 07 | Add computer: compare | Flow B: both identities, big code, compare instruction | waiting for approval; approved; denied; expired |
| 08 | Add computer: waiting | Flow A: identity checked by the QR, code shown smaller | waiting with countdown; another device already waiting |
| 09 | Add computer: success | Paired, with what is shared | shares listed; nothing shared yet |
| 10 | Add computer: failure | Why it stopped and what to do on the computer | code mismatch; denied; expired; pairing closed; rate limited; wrong computer; secure storage failed |
| 11 | Shares | Folders shared with this phone and what each allows | loaded; nothing shared; offline (cached chip); share removed |
| 12 | Browser: list | Folder with breadcrumb, Files/Recent, sort and kind chips | loaded; loading (skeleton rows); empty; filter matches nothing; partial search results; page error with Retry |
| 13 | Browser: grid | Thumbnails made on the computer | ready; generating; no thumbnail on host (glyph + reason); online-only badge |
| 14 | Sort and filter | Bottom sheet; remembered per share | default |
| 15 | Multi-select | Count, size, Fetch many | some selected; fetch not allowed by share; folders not selectable |
| 16 | Browser: offline | Cached listing with its age | offline with cache; offline without cache; partially cached |
| 17 | File sheet | Three modes, cost hints, work rail, default | all available; saved on phone (Fetch becomes "Open the saved copy"); loading details |
| 18 | File sheet: unavailable | Every unavailable mode with its reason | no renderer; host locked; metered and large; not allowed by share; refused by policy; stream busy; relay warning |
| 19 | Preview: pages | Pages rendered on the computer, filmstrip | rendering a page; low fidelity (no Office renderer); failure with Fetch / Stream |
| 20 | Preview: image | Screen-sized copy, original costs a Fetch | loaded; loading |
| 21 | Preview: text | Monospace, encoding shown | loaded; load more; capped; empty file |
| 22 | Preview: video | Plays from the computer by Range | poster; playing; codec not supported; online-only source |
| 23 | Downloads | All fetches | empty; queued; downloading; verifying; done (verified) with Open with / Share / Keep offline; failed (mismatch, gone, no space); paused by Android |
| 24 | Mobile data dialog | Size first, cheaper modes offered | alternatives available; alternatives unavailable with reason |
| 25 | Storage | What is saved, per computer | normal; kept-offline over the cap; empty; removed computer |
| 26 | Settings | Downloads, storage, opening rules, security, computers, about | default |
| 27 | App lock | Biometric gate | locked; declined; no screen lock any more |
| 28 | Stream: relay warning | Quality expectation before a relayed stream | Tailscale wording; generic wording |
| 29 | Stream: window picker | Choose among the app's windows within a countdown | several windows; none yet; picked window gone |
| 30 | Stream: live, Direct touch | Session bar, mode switch, keyboard bar | asking; approving on host; launching; live; sticky Ctrl latched |
| 31 | Stream: Trackpad | Pointer mode with gesture card | first use; after |
| 32 | Stream: paused | Frozen, greyed frame with reason | someone at the computer; another window in front; weak connection; app in background |
| 33 | Stream: reconnecting | Session kept 120 s, countdown | reconnecting; gave up |
| 34 | Stream: end dialog | Keep or close the app; fetch the saved file | user ends; ended by kill switch, lock, revoke, window closed, timeout, no video path |

### 10.2 Android tablet (Galaxy Tab S9 class, 1280 x 800 dp landscape)

| # | Screen | Purpose | States |
| --- | --- | --- | --- |
| 35 | Two-pane browser | Navigation rail, folder pane (440 dp), file detail pane with first-page preview and the three actions side by side | file selected; nothing selected (folder summary); portrait falls back to one pane with the bottom sheet |
| 36 | Stream | Full-width window, session bar on top, one-row keyboard bar | live; hardware keyboard attached (bar hidden, modifiers kept in the session bar) |

All other phone screens reuse their phone layout centred at a 600 dp max width on tablets, except
Computers (two-column host grid) and Downloads (list plus detail), which are noted for the
implementation plan but not mocked.

### 10.3 Windows host

| # | Screen | Purpose | States |
| --- | --- | --- | --- |
| 37 | Tray flyout and menu | Status, who is connected, quick actions, kill switch; minimal right-click menu | idle; connected; live (orange); paused (grey, time left) |
| 38 | Shares | Master-detail share list, options, per-device permission grid, Stream caution | shares; none ("Nothing is reachable yet"); folder missing; unsaved changes |
| 39 | Share a folder | Dialog; every device starts unticked, Stream never set here | new; whole drive or profile chosen (warn) |
| 40 | Devices | Identity, address, path, activity, Stop / Revoke; shown while live | live band; none; revoked list |
| 41 | Add a device: QR | QR, three steps, manual addresses, expiry, failed attempts | open; rate limited; expired |
| 42 | Add a device: approve | Identity, tailnet login, large code, Deny as wide as Allow | Tailscale identity; identity unknown; shared from another tailnet; tagged node refused |
| 43 | Activity | Audit log in plain words | events; only problems; nothing yet |
| 44 | Network | Listening addresses, adapter, per-device path, connectivity check with fixes | all good; Shields Up; firewall block rule; public network note; no private network (loopback only) |
| 45 | Settings | General, streaming, listener, storage | default |
| 46 | Being viewed | Pill, toast, tray dot, OS capture border | streaming; browsing only (toast, no pill) |
| 47 | Stream prompts | App approval and attended-computer prompts as notifications | app not approved; someone is at the computer |
| 48 | Kill switch used | Result stated, undo is Resume now | just used; paused from the menu |

## 11. Accessibility

- Contrast as in 3.2; orange text always uses `accent-text`, never `accent`.
- Every icon button has a content description / accessible name; the work rail has the caption as
  its label and the glyph is decorative.
- The pairing code is announced digit by digit with a pause between groups ("4 8 2, 9 1 3").
- Disabled actions keep their reason at full contrast and focusable, so TalkBack reads why.
- Stream mode: Direct and Trackpad are labelled buttons, not hidden gestures; the keyboard bar keys
  have names ("Control, sticky, on").
- Host: keyboard navigation through the nav pane and the permission grid; the live band and the
  pill are announced by Narrator when they appear (accesskit live region).

## 12. Open questions for the owner

1. **Dynamic colour on Android 12+:** off (fixed Gander palette, recommended) or on (wallpaper
   colours; the orange rules would then only partly hold)?
2. **Orange as selection colour:** keep orange for selection and focus too, or reserve it strictly
   for live and default and use ink for selection?
3. **Navigation:** a bottom nav bar with Computers / Downloads / Settings (mocked) versus the
   spec's single home screen with Downloads and Settings as top-bar icons.
4. **Allow delay on the host:** make "Allow" on the pairing approval inactive for the first 2-3
   seconds so it cannot be a reflex click? Not mocked.
5. **Visible label for the kill switch:** "Disconnect everyone" (mocked) or keep "Kill switch".
6. **Host icons:** Segoe Fluent Icons from the system (most native) or the same Material Symbols
   as Android (one set, bundled, licence already permissive)?
7. **Tablet sheet:** on the 12.7" tablet, should the detail pane always show, or only after a file
   is tapped (more room for the list)?
8. **Global hotkey for the kill switch** (for example Ctrl+Alt+Win+G)? Not in any spec today.
9. **Real Mica later?** The flat approximation is safe; true Mica needs a transparent viewport and
   DWM attributes, which is a small spike on Windows 11 only.
10. **Stand-in app imagery:** the mockups draw a generic presentation app for Stream; confirm that
    PowerPoint remains the reference app for the real-application checklist (WP-57).
