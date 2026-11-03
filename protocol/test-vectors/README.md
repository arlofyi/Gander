# Protocol test vectors

Machine-readable fixtures shared by every host implementation (Rust first; macOS and Linux later)
and, where relevant, by client test suites. Add new vector files here rather than hard-coding
cases in one language's tests.

## `paths.json`

Hostile and benign protocol paths for the resolver specified in
`docs/specs/2026-11-01-path-resolution.md`.

Template tokens inside `input` and fixture values:

| Token | Meaning |
| --- | --- |
| `{{segment:'a'*100}}` | the character repeated N times |
| `{{repeat:'d/',129}}` | the string repeated N times |
| `{{bytes:0xC3,0x28}}` | raw bytes (used for invalid UTF-8; the test harness must pass bytes, not a Rust `str`) |
| `{{outside}}` | a temp directory created outside the share root |
| `{{root}}` | the share root |
| `{{same}}` | the same value as `input` |

Fixture keys: `files` (created with small content), `dirs`, `symlinks` (`dir: true` for directory
links), `junctions` (Windows), `reparse` (creates a file carrying the named reparse tag where the
harness can; otherwise the vector is skipped), `attrs` (Windows attributes per name), `share`
(share-level options for this vector), `race` (a described mutation the test performs after
`resolve` returns).

Vectors carrying `privilege` are skipped with an explicit message when the fixture cannot be
created in the current environment. Silent skips are a test bug.

## `policy.json`

Stream launch policy vectors for `docs/specs/2026-11-01-stream-launch-policy.md` ("Policy: who
may open what"). Each vector names a file; the test resolves its extension against the fake
association table `associations` (or the vector's own `handler`), applies the app catalogue
`catalogue` (the fields of `apps.toml`) and checks `expect` (`allowed | refused | ask`), the
decision path `reason` and, for browser handlers, `browser` (the isolated-window launch).
Optional per-vector keys: `share_default`, `stream_permitted`, `app` (catalogue id the client
asked for), `handler` (`null` means nothing is registered). The Rust runner is
`host/gander-core/tests/policy_vectors.rs`.

## `sas.json`

Short authentication string vectors for pairing (`docs/specs/2026-11-01-pairing-and-trust.md`,
flow A and B). Each vector gives the host's DER `SubjectPublicKeyInfo` (`host_spki_der_hex`), both
nonces (hex and base64url), the intermediate SHA-256 and `first_20_bits`, and the expected code
(`sas_digits`, display form `sas`). Byte-level choices the spec leaves open are fixed in the file's
`description`: the SPKI enters the hash as DER bytes (not its hash), nonces as raw bytes, and the
20-bit value is reduced modulo 1 000 000 before zero-padding. `sas-mitm-other-spki` uses another
key with the same nonces and must give a different code. The vectors were generated with an
independent implementation (Python `hashlib`); the Rust runner is
`host/gander-core/tests/sas_vectors.rs`, the Android client (WP-22) reads the same file.

## `tiles.json`

Deep-zoom tile pyramid vectors (WP-67, `docs/specs/2026-11-01-preview-renderers.md`, "Deep-zoom
tiles"). Each vector gives the top-level size of a page (`top`, pixels) and the expected number of
`levels` (null when the page cannot be tiled), the size of every level (`level_sizes`), the tile
grid of every level (`grids`, columns and rows) and sample tiles by `level`, `x`, `y`: `expect` is
null when the tile does not exist, else the tile image `size` and the area it shows, `src` (x, y,
width, height in top-level pixels, clipped to the page). The Rust runner is the unit test in
`host/gander-core/src/tiles.rs`; the Android client reads the same file (`TilePyramidTest`,
property `gander.tileVectors`).
