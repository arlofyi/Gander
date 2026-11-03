# gander-testwin

A deterministic Win32 test window for the stream spike (capture, encode, input injection) and the
Phase 2 end-to-end tests. It paints known content at 60 Hz so that a capture, a photograph or a
test can check what was actually displayed.

```
cargo run -p gander-testwin -- [--self-test | --self-test-secs N] [--no-focus]
    [--splash MS] [--reuse KEY] [--twin] [--dpi-unaware] [--title TEXT | TITLE...]
```

- Title comes from argv (`--title "Spike A"` or bare words); default `Gander Test Window`.
- `--self-test` exits with code 0 after 20 s, `--self-test-secs N` after N s (1..=3600). Always use
  one of them in unattended runs. The exit also works while the popup menu or a dialog is open
  (the clock thread closes them and repeats the quit request every frame); if the window thread
  still does not leave within three seconds, the process force-exits with code 4.
- `--no-focus` shows the window without activating it.
- Window discovery scenarios (WP-51, `tests/discovery.rs`), all optional and off by default:
  - `--splash MS` first shows a visible popup of class `MsoSplash` (Office's splash class, so the
    production skip list is what ignores it) with the same title, for MS milliseconds
    (1..=10000), then the main window.
  - `--reuse KEY` makes the process single-instance per KEY (`[A-Za-z0-9_-]`, at most 32). If an
    instance with that KEY runs, the new process sends it `open-second <TITLE>` over
    `\\.\pipe\gander-testwin-reuse-<KEY>`, prints
    `gander-testwin forwarded key=<KEY> reply=ok hwnd=0x<hex> title=<TITLE>` and exits 0 without
    opening a window. Otherwise it becomes that instance: it also serves the reuse pipe (same
    protocol and access rules as the per-pid pipe) and prints the normal ready line.
  - `--twin` opens a second window titled `<TITLE> (second)` right after the main window (two
    discovery candidates).
- `--dpi-unaware` (WP-52 input tests) runs the process DPI-unaware instead of per-monitor v2: on a
  scaled monitor the system bitmap-stretches the window and gives it logical (96-dpi)
  coordinates, as for an old desktop application. `layout?`, `state?` and `last_click` are then
  in those logical units.
- On start it prints one line on stdout (this line is an interface; parse it):
  `gander-testwin ready pid=<pid> pipe=\\.\pipe\gander-testwin-<pid> hwnd=0x<hex> title=<title>`
- Exit code: 0 clean (window closed, `quit`, or self-test), 1 start-up failure, 2 usage error,
  4 forced exit after a hung self-test.

## What is on screen

The window is resizable and per-monitor DPI aware (v2), initially 640x480 client pixels at 96 dpi
and scaled by `dpi / 96` (so 960x720 at 150 %). All coordinates below are **client-area pixels**,
origin at the top-left of the client area, logical sizes at 96 dpi. A window capture (WGC) includes
the title bar and borders, so add the client origin before sampling; the library helpers take a
sampling closure for exactly that reason.

| Element | Position and content |
| --- | --- |
| Patch A | Top-left corner, 64x64, solid **RGB(255, 0, 255)** (magenta), exact in every pixel |
| Patch B | Bottom-right corner of the client area, 64x64, solid **RGB(0, 255, 0)** (green), exact |
| Frame counter, decimal | 8 digits at (80, 8), dot font, 6 px per dot, white on dark; value is `frame & 0xFFFFFF` |
| Clock | `HH:MM:SS.mmm` local time at (80, 58), 4 px per dot |
| Frame counter, binary | 24 cells of 16x16 at (8, 96), most significant bit first; black (0,0,0) = 0, white (255,255,255) = 1 |
| Arena | From y = 128 to (client height - 64), full width, fill (44,44,56) with a 1 px border (96,96,120) |
| Box | 64x64 square inside the arena; speed 2 px/frame horizontally and 1 px/frame vertically, reflecting at the edges (triangle waves, so its position is a pure function of the frame number); colour starts at (255,128,0) |
| Caption | Bottom left (GDI text: title, pid, hits/clicks); never covers the patches |

The patches are drawn last and the caption is clipped, so nothing can cover them. They are exact in
the window's own back buffer; after video coding compare with a tolerance (`patch_matches` takes
one). For an occlusion test (C2) check that both patches are still magenta and green.

The frame number is `floor(elapsed_since_process_start * 60)`, published by a dedicated 60 Hz
thread, so it is wall-clock exact. If the window thread is late the counter **skips** numbers
instead of repeating them: a gap in captured counters is a dropped frame. While the window is
minimised it stops painting (the counter resumes ahead of where it left off). The animation keeps
running inside the popup menu, the message box and while the window is dragged or resized.

Box colour palette (a click on the box advances to the next, wrapping): `FF8000`, `0080FF`,
`FFFF00`, `FF0000`. A click anywhere else only counts as a click.

Reading values back (`use gander_testwin::{layout::Layout, render::{decode_counter, patch_matches}}`):

```rust
let layout = Layout::new(client_w, client_h, dpi);
let frame = decode_counter(&layout, |x, y| sample_client_pixel(x, y)); // Option<u32>, = frame & 0xFFFFFF
let ok = patch_matches(layout.patch_a, PATCH_A_RGB, 0 /* tolerance */, |x, y| sample_client_pixel(x, y));
let box_now = layout.box_rect(frame);
```

A process that reads the window's size or pixels must itself be per-monitor DPI aware; otherwise
Windows virtualises the client rectangle (640x480 reported for a real 960x720 window at 150 %) and
every position is wrong. `PrintWindow(hwnd, hdc, PW_CLIENTONLY)` works and returns the back buffer.

## Input

- **Left click** on the box changes its colour (`WM_LBUTTONDOWN`, so `SendInput` and posted
  messages both work). `colour?` / `state?` report the colour, `hits` (clicks on the box), `clicks`
  (all clicks) and `last_click`.
- **Ctrl+P** shows a tracking tooltip and a popup menu (Cycle box colour, Open second window,
  Quit) at fixed spots in the arena; **Ctrl+D** opens a modal message box. Both arrive as
  `WM_CHAR` control codes 0x10 and 0x04, so a posted `WM_CHAR` works without keyboard focus (a
  posted `WM_KEYDOWN` would not: Ctrl is read from synchronised key state by most apps, this one
  does not rely on it).
- Closing the main window ends the process; closing a second window does not.

## Control pipe

One pipe per process: `\\.\pipe\gander-testwin-<pid>`. Created with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
remote clients rejected, and a protected DACL with a single allow entry for the current user's SID
(no SYSTEM, no Administrators). One client at a time; a second `CreateFile` gets
`ERROR_PIPE_BUSY` (231), so retry. A client that sends nothing for 60 s is dropped.

Line protocol: one UTF-8 request line (`\n` or `\r\n`, at most 256 bytes) gets exactly one reply
line, `ok [key=value ...]` or `err <code> [detail]`. Any number of requests per connection.

| Request | Effect | Reply |
| --- | --- | --- |
| `ping` | nothing | `ok pong` |
| `state?` | snapshot | `ok frame=N colour=RRGGBB hits=H clicks=C last_click=X,Y\|none windows=N minimised=0\|1 menu=0\|1 tooltip=0\|1 dialog=0\|1 box=X,Y,W,H` |
| `colour?` / `color?` | box colour and counters | `ok colour=RRGGBB index=I hits=H clicks=C` |
| `typed?` | keyboard input received (WP-52) | `ok keydowns=K chars=N text=<hex of the UTF-8 of the last 256 UTF-16 units, `-` when none>` |
| `frame?` | last frame the main window painted | `ok frame=N` |
| `layout?` | geometry of the main window (same numbers as `Layout::new`) | `ok dpi=D client=WxH patch_a=X,Y,W,H patch_a_rgb=FF00FF patch_b=X,Y,W,H patch_b_rgb=00FF00 strip=X,Y,W,H strip_cell=P strip_bits=24 arena=X,Y,W,H box_size=P counter_text=X,Y,SCALE clock_text=X,Y,SCALE palette=RRGGBB,...` |
| `windows?` | window handles, main first | `ok count=N hwnd=0xAAAA,0xBBBB` |
| `open-second [TITLE]` | opens a second window of the same process, same class and content; default title `<title> (second)` | `ok hwnd=0xBBBB` |
| `minimise` / `minimize` | minimises the main window | `ok` |
| `restore` | `SW_RESTORE` on the main window | `ok` |
| `popup` | same as Ctrl+P | `ok` (returns at once; the menu is modal on the window thread) |
| `dialog` | same as Ctrl+D | `ok` (returns at once) |
| `dismiss` | closes the menu, tooltip and any dialog | `ok` |
| `freeze` | stops the animation: no repaint, static content (WP-50 keepalive test) | `ok` |
| `park` | `freeze`, and every paint shows frame 0: the box sits at its start position (top-left of the arena) whatever repaints the window, so a harness knows where it is (WP-57 `e2e-stream`) | `ok` (after the repaint) |
| `animate` | restarts the animation after `freeze` or `park` | `ok` |
| `tone [HZ [MS]]` | hook for the audio work package, see below | `err not-implemented` |
| `quit` | clean shutdown, exit code 0 (also with a menu or dialog open) | `ok`, sent before the shutdown |

Errors: `err unknown-command`, `err bad-argument <detail>`, `err timeout` (window thread did not
answer within 2 s), `err line-too-long` (connection then closed), `err failed <detail>`,
`err not-implemented`.

`open-second`, `minimise`, `restore`, `dismiss` and `park` are synchronous: the reply comes after the
window thread has done the work. `popup` and `dialog` are posted because they block the window
thread inside a modal loop until dismissed.

Example (PowerShell): connect with `System.IO.Pipes.NamedPipeClientStream('.', "gander-testwin-$pid")`,
write `ping`, read a line. Rust: `OpenOptions::new().read(true).write(true).open(pipe_name)`.

### Audio hook (not implemented)

`tone` is parsed and range-checked (HZ 20..=20000, default 440; MS 1..=10000, default 500) and
routed to `src/win/audio.rs::play_tone`, which returns `NotImplemented`. The audio work package
replaces that function body (it must return as soon as playback is queued) and the reply becomes
`ok`. Nothing else needs to change.

## Layout of the crate

- `src/lib.rs`: portable library (`layout`, `render`, `protocol`, `cli`; `sec` on Windows). Other
  crates (the capture spike) can depend on it to reuse the geometry and the pixel readers.
- `src/main.rs`, `src/win/`: the Win32 runtime (window thread, 60 Hz ticker thread, pipe thread).
  Compiled only on Windows; elsewhere the binary prints an error and exits 2.
- `tests/discovery.rs`: window discovery, fit-to-phone and restore through `gander_win::launch`
  against real test-window processes (new process, re-used process via the reuse pipe,
  splash-then-main, two candidates, every monitor's DPI). Run with `--nocapture` to see the fit
  sizes per monitor.
- `tests/window.rs`: end-to-end test that launches the real window, drives the pipe and reads the
  pixels with `PrintWindow`. `GANDER_TESTWIN_DUMP_DIR=<dir>` saves each captured shot as a PPM.
  The `#[ignore]`d `ctrl_p_opens_popup_menu_and_tooltip` sends real keystrokes and needs the
  desktop foreground.
