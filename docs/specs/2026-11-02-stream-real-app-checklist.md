# Stream mode with real applications: owner checklist (WP-57)

_Written 2026-11-02 for the owner. Status: **to run by hand**; nothing below has been run yet._

`cargo xtask e2e-stream` proves the Stream path end to end with `gander-testwin`, a test window
built for the purpose. It cannot tell whether streaming is pleasant with real applications. That
needs a person, a real tablet and real files, so this checklist is the human half of WP-57. It
also covers what the harness defers while the laptop is locked (input injection, a tap that
changes something on the phone).

Fill in the results table at the end and commit the file. Anything that fails goes to
`docs/context.md` as an open item. Note the exact app version, because the results depend on it.

## Before you start

1. **Laptop:** unlocked, on mains power, both monitors connected as usual. Note the display
   scaling of the monitor the apps open on (Settings > System > Display, for example 150 %).
2. **Host:** run the current build with `cargo xtask run-host`, or start the installed Gander.
   The tray icon shows. Open the Gander window and check that the tablet is paired and that one
   share holds the test files below, with **Stream** allowed for the tablet.
3. **Tablet** (an Android tablet): Gander client installed, on the tailnet (Tailscale app
   connected), the host listed as online.
4. **Test files** in the shared folder. Use copies, not originals: Stream opens them in the real
   app and you may edit them by accident.
   - `checklist/Deck.pptx`: a 5-10 slide deck with small body text (12-14 pt), one image and one
     slide with an animation or transition.
   - `checklist/Manual.pdf`: a multi-page PDF with small print (a datasheet or manual).
   - `checklist/Photo.png`: a large photo (at least 3000 px wide) with fine detail.
   - `checklist/Notes.txt`: about 50 lines of plain text.
   - `checklist/Scene.blend`: any small Blender scene, **only if Blender is installed**
     (`where blender` or the Start menu). Skip the Blender row otherwise.
5. **First use of each app:** the host asks on the laptop screen "<tablet> wants to open
   `<file>` with <APP.EXE>. Allow this app for streaming?" Choose **Allow once** for this run.
   Note whether the prompt appeared and named the right program.
6. Close every copy of PowerPoint, Photos and Notepad on the laptop first, so each test starts
   the app fresh (this is what `close_on_end` is judged on). Leave Edge as it is: Gander opens PDFs
   in its own isolated Edge window, separate from your browsing.

## The same steps for every app

On the tablet:

1. Open Gander, the laptop, the share, then the file. In the file sheet, choose **Stream**.
2. Watch the phases the viewer shows (requested, approving if asked, launching, streaming).
   Start a stopwatch at the tap on **Stream** and stop it when the first picture shows: that is
   the **time to first picture**.
3. **Fit to phone:** on the laptop, the app window is resized to the tablet's shape while you
   stream. Note whether it fits the tablet's screen without large black bars, and whether the
   window stayed on the monitor it opened on.
4. **Text legibility:** at the default zoom, can you read body text without zooming? Then pinch
   to zoom on a paragraph: is it sharp after about a second (the encoder catches up), or blocky?
5. **Latency:** in Direct touch mode, drag something (scroll the page or pan the image) and
   watch the picture follow your finger. Rate it: _instant_ (under 150 ms, feels attached),
   _usable_ (noticeable but fine), _laggy_ (you wait for it). If you can, film the tablet and
   laptop together with another phone at 60 fps and count frames between your finger moving and
   the laptop screen moving, then the tablet picture moving.
6. **Input:** tap a button or menu in the app (it should react on the laptop), type a few words
   with the keyboard bar (where text input makes sense), use Trackpad mode once (the cursor
   moves relatively, a tap clicks). Note anything that went to the wrong place, was missed or
   arrived twice.
7. **Bring to front:** when the stream starts, the app should already be in front on the laptop
   (the host brings it forward once; if Windows refuses, the tablet asks once by itself). Then,
   on the laptop, click another window so the streamed app goes behind it.
   The tablet should say input is paused (focus lost) and offer **Bring to front**. Tap it: the
   app comes back to the front and input works again.
8. **Local user:** move the laptop's own mouse while streaming. The tablet should say someone is
   using the computer, and input from the tablet pauses for about 2 s. The picture keeps flowing.
9. **Being viewed:** check the laptop shows that it is being streamed (tray icon in the
   streaming state, a toast with **Stop**).
10. **End:** on the tablet, end the stream. If asked whether to close the app, choose **Close**
    once (for one app) and **Leave open** for the others, and note which.
11. **Restore placement:** on the laptop, the app window should be back at its original size and
    place (if it stays open). **close_on_end:** if Gander started the app and you chose Close (or
    the app's setting is `if_launched_by_us`), the app should close; with unsaved changes it
    must stay open and the tablet must say so.
12. Once, for any one app, press the tray's **kill switch** (Stop everything) during a stream
    instead of ending it from the tablet: the stream must stop at once and the tablet must show
    it ended.

## What to look for per app

| App | File | Specific checks |
| --- | --- | --- |
| **PowerPoint** | `Deck.pptx` | Opens in normal (editing) view; the slide thumbnails and small body text are readable after fit to phone; arrow keys from the keyboard bar change slides; start the slide show (F5 from the keyboard bar or the ribbon) and check the full-screen slide show window is what streams (or note that the stream stayed on the editing window); the animation plays smoothly on the tablet; on end, PowerPoint asks to save only if you changed something |
| **Edge PDF (isolated window)** | `Manual.pdf` | The phone says "opens in Microsoft Edge (isolated window)"; the window that opens has no tabs, bookmarks or extensions of your own profile (Gander's own Edge profile), and your other Edge windows are untouched; small print readable after zoom; scrolling with a finger is smooth; Ctrl+F from the keyboard bar finds text; on end the isolated window closes (if close was chosen) and your own Edge stays open |
| **Photos** | `Photo.png` | Opens in Microsoft Photos (a packaged app: this was broken until the 2026-11-02 device fixes); the picture shows; pinch zoom on the tablet versus zoom inside Photos both work; fine detail is not smeared when static (the keepalive repeats sharpen it); on end Photos restores its size |
| **Notepad** | `Notes.txt` | Opens in Notepad (packaged, from `WindowsApps`); type a sentence from the keyboard bar, including an accented letter such as é and an emoji, and check what arrives; select text by dragging; on end with unsaved changes Notepad stays open and the tablet says it was left open |
| **Blender** (if installed) | `Scene.blend` | Slow start is tolerated (up to 30 s); the viewport renders on the tablet; orbit with a one-finger drag in Direct touch (middle mouse is not available: note how usable it is); a heavy viewport keeps a steady frame rate or degrades gracefully (note the fps the stream shows, if shown); the GPU load on the laptop (Task Manager) while streaming |

## Results

One row per app. Times in seconds, latency as instant / usable / laggy (or the measured
milliseconds). For yes/no columns write `yes`, `no` or a short note.

| App (version) | Prompt shown and right | Time to first picture (s) | Fit to phone | Text legible (default / zoomed) | Latency | Taps, typing, trackpad | Bring to front | Local-user pause | Being-viewed indicator | Restore placement | close_on_end (choice, result) | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| PowerPoint ( ) | | | | | | | | | | | | |
| Edge PDF, isolated ( ) | | | | | | | | | | | | |
| Photos ( ) | | | | | | | | | | | | |
| Notepad ( ) | | | | | | | | | | | | |
| Blender ( ) or "not installed" | | | | | | | | | | | | |

Kill switch (which app): ______ stopped at once: ____ tablet showed the end: ____

Machine: Windows build ______, display scaling ______, GPU the stream used (shown in the host
log as `source: <adapter> / <encoder>`): ______. Tablet: ______, Android ______. Network:
same Wi-Fi / tailnet direct / DERP relayed (Tailscale shows which): ______.

## Also run while you are at the laptop

These are the automated checks that need an unlocked, attended session. Run them from the
repository root while you do not touch the mouse or keyboard (they move the cursor and type into
`gander-testwin` windows only):

```text
cargo xtask e2e-stream --android
cargo test -p gander-testwin --test input -- --ignored --nocapture --test-threads 1
```

`e2e-stream` prints each step; with the session unlocked nothing should be `deferred` except
what it names. The Android stage boots the `gander` emulator, so close any other emulator first.
