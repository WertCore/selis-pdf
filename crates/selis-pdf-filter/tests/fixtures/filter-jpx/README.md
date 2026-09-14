# Corpus `filter-jpx` — malformed JPXDecode regression set (SL-1.FILT.08)

Every codestream here is a **deliberate corruption of `../jpx_gradient.j2k`**, the
lossless 64×48 RGB J2K codestream the native `third-party/refjpx.c` oracle emits
(and which the sandbox decodes bit-exactly). Starting from a real, well-formed
OpenJPEG codestream — rather than random bytes — makes each case exercise the
specific parser structure it is named for, so a regression has to break that
structure to slip past.

They are all *contained*: decoded through `jpx_decode` behind the Tier-2
sandbox, each must return a typed error (sandbox/image family), never a host
crash, never unbounded host memory, and the caller's budget must stay usable
afterwards. The suite is `filter_jpx_corpus_is_contained` in
`crates/selis-pdf-filter/src/jpx.rs`.

`jpx_gradient.j2k` is big-endian J2K markers:

| offset | field |
|---|---|
| 8 | `Xsiz` (canvas width) |
| 12 | `Ysiz` (canvas height) |
| 24 | `XTsiz` (tile width) |
| 28 | `YTsiz` (tile height) |
| 50 | start of the tile-part bitstream |

Regenerate any entry from `jpx_gradient.j2k` with these recipes:

| file | recipe | hostile property exercised |
|---|---|---|
| `siz-bomb.j2k` | `Xsiz`=`Ysiz`=65535 | read_header image-alloc claim of 4×10⁹ px against a 2 KB tile — the memory cap, not the host, must answer |
| `tile-count-bomb.j2k` | `Xsiz`=`Ysiz`=1024, `XTsiz`=`YTsiz`=1 → 10⁶ tile parts | tile-index array growth + truncated per-tile data |
| `truncated-tile.j2k` | first 45 % of the file, cut inside the bitstream | no `EOC`, incomplete tile → `InputTruncated`/`InputMalformed` |
| `marker-garbage.j2k` | `FF4F FF5F FFFF FF` header spliced before the template body byte 6 | an unparseable marker segment ahead of valid data |
| `header-only.j2k` | first 24 bytes (SIZ header only) | header parses, but there is no tile-part to decode |

These are synthetic (derived from our own oracle output), so — unlike the wild
corpora governed by `pdf-plan/06-CORPUS-POLICY.md` — they are safe to vendor:
no third-party content, no provenance fetch, redistributable under the crate's
own Apache-2.0 licence.
