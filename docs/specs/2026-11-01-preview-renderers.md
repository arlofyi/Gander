# Spec: thumbnails and previews (host-side rendering)

- Status: Draft for implementation (Phase 1, WP-15 / WP-16)
- Date: 2026-11-01
- Related: `2026-11-01-protocol-v1.md` (thumb/preview endpoints), ADR-0007 (process model)

## Principles

1. **The listing never waits for a renderer.** Thumbnails are requested per visible row and
   served from cache or `503 generating`.
2. **Renderers parse untrusted bytes.** A share may contain a downloaded PDF or image crafted to
   exploit a decoder. Every renderer runs in a separate worker process with a job object (memory
   cap 512 MiB, CPU time cap 20 s, no child processes, below-normal priority), killed on timeout.
   The host process itself never links an image or PDF decoder.
3. **"The computer, a little."** Rendering is capped so the owner at the keyboard does not notice:
   thumbnail queue concurrency 2, preview queue concurrency 2 overall and 1 per device, worker
   priority below normal, and all work paused when the host reports `on_battery` below 15 %.
4. **Cache keyed by content identity**, not by path: (volume id, file id, size, mtime, renderer
   version, output size). Moving or renaming a file keeps its cache; editing it invalidates.

## Renderer catalogue (Windows host, v1)

| Kind | Inputs | Renderer | Notes |
| --- | --- | --- | --- |
| image | jpg jpeg png gif bmp webp tiff heic heif avif jxl | Windows Imaging Component (WIC) via the `windows` crate; falls back to the `image` crate for formats WIC lacks | HEIC/AVIF need the OS codec packs; report `unsupported_type` if the decoder is missing. EXIF orientation applied; ICC converted to sRGB; metadata stripped from output. |
| raw | cr2 cr3 nef arw dng raf orf rw2 | Embedded JPEG preview extracted directly (fast path, most RAW files carry one); else WIC with the Raw Image Extension if installed | Never demosaic in v1. |
| pdf | pdf | `Windows.Data.Pdf.PdfDocument` (WinRT, in the box since Windows 10) rendering pages to BGRA at the requested width | Encrypted PDFs that need a password -> `unsupported_type`. Page count comes from the same API for `preview/info`. |
| text | txt md log json yaml toml csv ini xml html css js ts py rs go java c cpp h cs sql sh ps1 and any file that sniffs as text | Encoding detection (BOM, valid UTF-8, UTF-16 LE/BE, else windows-1252) and a bounded read | Highlighting is the phone's job. `preview/text` only; no bitmap. |
| office (thumbnail) | docx xlsx pptx odt ods odp vsdx | Embedded `docProps/thumbnail.(jpeg|emf|wmf)` from the zip container; EMF/WMF via GDI+ | Present only if the author saved with a thumbnail; frequently absent. Used for list thumbnails, not page previews. |
| office (render) | docx xlsx pptx | **Office COM automation** when Office is installed (PowerPoint `Presentation.Slides(n).Export`, Word `ExportAsFixedFormat` to PDF then the pdf renderer, Excel `ExportAsFixedFormat`); else **LibreOffice** `soffice --headless --convert-to pdf` when found; else none | COM runs in the worker process on an STA thread, with a hidden application instance that is quit afterwards. Office that is already open interactively is never touched: the worker always creates its own instance (`CoCreateInstance`, not `GetActiveObject`). Rendering a 50-page deck can take 10-20 s cold; `preview/info.estimated_ms` tells the phone. |
| video poster | mp4 mkv mov avi webm m4v | Shell thumbnail (`IShellItemImageFactory`, which uses the system's media thumbnail handler) at 512 px; if absent and `ffmpeg` is on PATH, one frame at 10 % duration | No transcoding in v1; Fetch with Range handles playback. |
| audio | mp3 flac m4a ogg wav | Embedded cover art via the shell thumbnail handler | Metadata (title/artist/duration) via `Windows.Storage.FileProperties` for `stat?meta=1`. |
| shell fallback | anything with a registered thumbnail provider (.blend, .psd, .ai, .svg, .sketch, .skp ...) | `IShellItemImageFactory::GetImage` with `SIIGBF_THUMBNAILONLY \| SIIGBF_BIGGERSIZEOK` in the worker | Third-party thumbnail providers crash routinely; the worker isolation exists for them. Icon-only results (flag `SIIGBF_ICONONLY` would return an icon) are rejected: no thumbnail is better than a generic icon. |
| folder | directory | None in v1; the client draws a folder glyph | A "first four thumbnails" mosaic is a later nicety. |

Preview kinds advertised per file follow this order: `pdf` for PDFs; `image` for images and RAW;
`office` when a render path exists, else `none` with a thumbnail if embedded; `text` for text;
`video_poster` for video; `none` otherwise.

## Sizes and formats

- Thumbnails: longest side 128, 256 or 512 (client picks by density); WebP quality 80, JPEG
  fallback for clients that cannot decode WebP (all targets can). Transparent images get a
  checkerboard-free alpha-preserving WebP.
- Previews: width up to 2048 (clamped), WebP quality 85 or JPEG 88; a PDF page at width 1080 is
  roughly 120-250 KB. Pages are rendered one at a time; the client prefetches n+1.
- Large images (over 40 MP) produce the preview from a downscaled decode (WIC scaler) so memory
  stays under the worker cap; the client can still Fetch the original.

## Deep-zoom tiles (WP-67)

A page rendered at screen width (at most 2048 px) is blurry under deep zoom: a 6000 x 4500 photo or
an A1 drawing at 300 dpi has several times the pixels. `GET /preview/tile` serves the page as
512 x 512 WebP tiles on a pyramid; the geometry is `gander_core::tiles` and
`protocol/test-vectors/tiles.json`.

- **Pyramid.** The top level is the natural resolution: an image's own pixels (after EXIF
  orientation), a PDF page at 300 dpi (reduced when the longest side would pass 32768 px, which
  also bounds images: larger ones have no tiles). Level 0 is the smallest level whose longest side
  fits in 512; each level is twice the one below (rounded up) up to the top. Tiles are cut from the
  top left, tiles on the right and bottom edge are smaller, never padded.
- **Rendering.** `RenderOp::Tile { page, level, x, y }` in the worker. Images: the tile's area is
  mapped back to stored pixels (EXIF orientation), cut out with a WIC bitmap clipper and scaled
  to the tile size, so the codec reads only the area it needs (measured: a top-level tile of a
  50 MP JPEG about 100 ms against about 240 ms for the whole picture at level 0); the whole-image
  tile at level 0 goes through the scaler alone (JPEG reduced-size decode). Formats WIC lacks fall
  back to the `image` crate, which decodes in full (within the decoder limits). RAW previews are
  tiled from the embedded JPEG. PDF: `PdfPageRenderOptions.SourceRect` (page units, 1/96 inch)
  with the destination size set to the tile size; the renderer's oversize quirk (display scale)
  is corrected by scaling the result, or asking again when it came back small. Word, Excel and
  LibreOffice documents go through the cached PDF export and are tiled as a PDF; a PowerPoint
  slide is exported as a picture, so it has no tiles in v1 (a PDF export of the deck would number
  hidden slides differently).
- **Scheduling.** Tiles share the preview queue (1 per device, 2 overall) as a low-priority
  class: a tile starts only when no page, info or metadata job is waiting for a slot (running
  jobs are never interrupted), a queued tile not asked for again within 15 s is dropped (the
  client cancels by no longer asking), and a full queue evicts tiles before pages.
- **Cache.** `previews/` entries keyed by file identity (volume, file id, size, mtime), tile
  renderer version, page, level, x and y; the 1 GiB cap and LRU eviction are shared with pages.
- **Info.** `preview/info` lists `tile_size`, `tile_levels` and `tile_pages` (top-level size and
  level count per page, at most 512 pages) when some page needs a second level. For Word, Excel
  and LibreOffice documents they appear once the PDF export exists (after the first page render).

## Cache

- Location `%LOCALAPPDATA%\Gander\cache\{thumbs,previews}\<2 hex>\<key>.webp`; index in a small
  embedded DB (redb or SQLite) with LRU bookkeeping. Default cap 2 GiB total, owner-configurable;
  eviction runs when 95 % full down to 80 %.
- Cache entries are never trusted as proof a file still exists: the ETag check on request re-stats
  the file through the resolver first.
- Purge button in the tray UI; purge on share removal.

## Worker protocol

- One worker executable `gander-render.exe`, launched per job with the file **handle duplicated
  into it** (the worker never receives a path, only an inherited handle and the job parameters
  on stdin), output image bytes on stdout, diagnostics on stderr. Exit codes: 0 ok, 2
  unsupported, 3 timeout/killed, 4 decoder error.
- The worker runs with a restricted token (SE_GROUP_INTEGRITY low, or an AppContainer when the
  renderer permits; Office COM does not, so the office worker runs at medium integrity but still
  inside the job object).
- Concurrency and queueing live in the host; the worker is single-job and exits.

## Scheduling

- Priority order: previews for the device's current file, thumbnails for the visible viewport
  (the client sends `priority=visible`), then prefetch.
- A request for a thumbnail that is already queued returns the same `503 generating` with the
  position-based `retry_after_ms` (200 ms x position).
- Scrolling fast generates many requests; the client cancels (closes) requests for rows that left
  the viewport, and the host drops queued jobs whose request connection closed.

## Metadata (`stat?meta=1`)

- Images: dimensions, orientation, taken_at, camera model (EXIF via WIC metadata reader).
- Video/audio: duration, dimensions, codecs via `Windows.Storage.FileProperties` or ffprobe if present.
- PDF: page count via `Windows.Data.Pdf`.
- Office: page count only if rendered already (from the PDF export); else null.
- All metadata reads happen in the worker as well.

## Tests

- Golden tests per renderer with small fixtures (a 3-page PDF, a rotated JPEG with EXIF, a PNG
  with alpha, a UTF-16 text file, a `.pptx` with and without an embedded thumbnail). Compare
  dimensions and a perceptual hash, not bytes.
- A deliberately malformed PDF and a decompression-bomb PNG must produce exit code 4 or 3 within
  the caps and never affect the host process.
- Cache key tests: rename keeps the entry; touch invalidates; cap enforcement evicts oldest.
- Office COM tests are skipped unless Office is detected, with an explicit skip message. On the
  development machine Office is installed, so they run there.
