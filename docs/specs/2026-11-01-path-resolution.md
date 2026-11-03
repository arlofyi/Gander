# Spec: path resolution and directory listing

- Status: Draft for implementation (Phase 1, work package WP-11 in `plans/2026-11-01-master-plan.md`)
- Date: 2026-11-01
- Implements: `security.md` non-negotiable "A phone path is never joined onto a root as a string"
- Test vectors: `protocol/test-vectors/paths.json` (machine-readable; the host test suite loads it)

## Why this exists

Every file request from a phone carries a share id and a share-relative path. If any request can
reach a byte outside the share root, the product's core promise is broken. This module is the single
place where a protocol path becomes an open file handle. Everything else (listing, thumbnails,
previews, fetch, stream launch) takes a handle from here, never a path string.

## Definitions

- **Share root**: an absolute directory path chosen by the owner. At share creation the host opens
  it and records its canonical form (`\\?\`-prefixed final path from `GetFinalPathNameByHandleW`
  on Windows; `realpath` on POSIX). The root itself MAY be a symlink or junction; it is resolved
  once, at creation, and re-verified each time the host starts.
- **Protocol path**: UTF-8, `/`-separated, share-relative, as received after exactly ONE
  percent-decoding by the HTTP layer. `""` or `"/"` means the root.
- **Segment**: a non-empty piece between separators.

## Algorithm (normative)

```
resolve(share, protocol_path) -> Handle | Reject(reason)

1. Decode: protocol_path is already percent-decoded once. Do NOT decode again.
2. Split on '/'. Drop a single trailing empty segment (trailing slash). Any other empty segment
   is Reject(empty_segment).
3. Depth check: more than 128 segments -> Reject(too_deep). Total length > 4096 bytes -> Reject(too_long).
4. For each segment, in order, BEFORE touching the filesystem:
   a. "." or ".." -> Reject(dot_segment)
   b. contains any of: NUL, chars 0x01..0x1F, backslash, ':', '<', '>', '"', '|', '?', '*'
      -> Reject(invalid_char). Backslash and ':' are rejected on every platform so behaviour is
      identical everywhere.
   c. ends with '.' or ' ' -> Reject(trailing_dot_or_space)      (Windows silently strips these)
   d. base name (before the first '.') case-insensitively equals one of
      CON PRN AUX NUL COM0-COM9 LPT0-LPT9 and the superscript variants COM1-3 / LPT1-3
      -> Reject(reserved_name)
   e. not valid UTF-8 -> Reject(invalid_utf8)
5. Start from the share root handle (opened at startup with directory semantics, no-follow).
6. For each segment:
   a. Open the child RELATIVE TO THE PARENT HANDLE, never by joined string:
      - Windows: NtCreateFile with ObjectAttributes.RootDirectory = parent handle,
        ObjectName = segment, options FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT,
        access FILE_READ_ATTRIBUTES plus FILE_LIST_DIRECTORY (dirs) or FILE_READ_DATA (files),
        share mode read|write|delete so we never block the owner's own apps.
      - Linux: openat2(parent, segment, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
        O_NOFOLLOW | O_CLOEXEC)
      - macOS: openat(parent, segment, O_NOFOLLOW | O_NOFOLLOW_ANY | O_CLOEXEC) then fstat
   b. Not found -> Reject(not_found). Access denied -> Reject(forbidden).
   c. Query attributes on the HANDLE:
      - If reparse point (Windows) or symlink (POSIX): read the reparse tag.
        ALLOW tags: cloud-files placeholders (OneDrive, Dropbox, Google Drive; the
        IO_REPARSE_TAG_CLOUD family), IO_REPARSE_TAG_WOF (compact.exe), IO_REPARSE_TAG_DEDUP.
        DENY everything else, including IO_REPARSE_TAG_SYMLINK, IO_REPARSE_TAG_MOUNT_POINT
        (junctions and volume mount points), IO_REPARSE_TAG_APPEXECLINK, IO_REPARSE_TAG_PROJFS and
        unknown tags -> Reject(reparse_point). Junctions INSIDE the share are also rejected in v1;
        a future option may allow links whose canonical target is under the root.
   d. Short-name check (Windows): read the real name of the opened item (FileNameInformation, or
      FindFirstFileW on the parent) and compare it to the requested segment using the volume's
      case rule. If they differ (requested `PROGRA~1`, real `Program Files`) -> Reject(short_name).
      This also catches trailing-dot/space normalisation if step 4c were ever bypassed.
   e. Intermediate segments MUST be directories; a file in the middle -> Reject(not_a_directory).
   f. Close the parent handle, continue with the child.
7. Belt and braces: on the final handle call GetFinalPathNameByHandleW(VOLUME_NAME_DOS |
   FILE_NAME_NORMALIZED) (POSIX: readlink /proc/self/fd/N, or F_GETPATH) and verify the result is
   the canonical root or starts with canonical_root + separator. Mismatch -> Reject(escaped) AND
   log at error level: it means a check above has a bug.
8. Return the handle. All subsequent I/O (ReadFile, directory enumeration, thumbnail input) uses
   this handle or a handle re-derived from it (ReOpenFile / dup). Never re-open by path.
```

Rejections are never detailed to the phone beyond the error code; the full reason goes to the
local audit log.

## Listing semantics

- Enumerate via the directory handle (`GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` on
  Windows; `fdopendir` on POSIX). Each entry reports: name, kind (`file` | `dir`), size, mtime,
  ctime (creation time on Windows), and attribute flags `hidden`, `readonly`, `system`,
  `cloud_only` (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS or RECALL_ON_OPEN), `link` (a reparse point
  of a denied class; shown greyed and not openable), `encrypted`.
- Entries whose names cannot be represented in UTF-8 (unpaired UTF-16 surrogates on Windows) are
  omitted and counted in `unlistable`.
- Hidden and system entries are omitted unless the share has `show_hidden = true`. `desktop.ini`,
  `Thumbs.db`, `.DS_Store`, `$RECYCLE.BIN` and `System Volume Information` are always omitted.
- Share-level `hidden_patterns` (globs matched against the share-relative path) remove entries
  from listings AND make them unresolvable (resolve returns not_found).
- Sorting and paging happen on the host over the full enumerated list. Directories with more than
  50,000 entries return the first 50,000 and `truncated: true`. The cursor is an opaque base64url
  of (sort key, name) of the last item returned.
- Case: names are returned exactly as stored. Two requests differing only in case may resolve to
  the same file on case-insensitive volumes; that is correct behaviour.
- Unicode normalisation: none. U+00E9 and `e` + U+0301 are different names on Windows; both are
  returned as stored and the client must not normalise either.

## Non-functional

- Resolving a 10-segment path completes in under 5 ms on a warm NTFS volume.
- Listing 10,000 entries completes in under 300 ms.
- No joined path string is allocated on the request path except for the step-7 comparison.

## Test obligations

- Load `protocol/test-vectors/paths.json`; for each vector create the described fixture inside a
  temporary share, call `resolve`, assert `expect` and `reason`.
- Fixtures needing privileges (file symlinks on Windows need Developer Mode or admin; junctions do
  not) are skipped with an explicit `SKIPPED (needs privilege)` line, never silently.
- A fuzz target feeds random byte strings and random valid-looking segment lists. Invariants: step
  7 never fires, and the function never panics.
- Step 7 has its own test: a deliberately broken resolver (string join) must be caught by it on a
  junction fixture.
- A listing test asserts hidden entries are omitted, `hidden_patterns` hide and block, and names
  come back byte-identical.

## Open items

- Linux and macOS implementations are Phase 3; vectors marked `platform: windows` are skipped
  there and a POSIX set (NUL in names, `/proc/self/fd` magic links, case-sensitive duplicates) is
  added.
- Policy option "allow links whose target is inside the share" is deferred until a user asks.
