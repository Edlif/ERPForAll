<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Optional report fonts: Chinese, Japanese and Korean

This directory is an optional font package for the native report renderer. It holds four families:

| Family | Font file | Bytes | Original upstream filename |
|---|---|---|---|
| Noto Sans SC | `NotoSansSC.ttf` | 17,772,300 | `NotoSansSC[wght].ttf` |
| Noto Sans TC | `NotoSansTC.ttf` | 11,941,968 | `NotoSansTC[wght].ttf` |
| Noto Sans JP | `NotoSansJP.ttf` | 9,589,900 | `NotoSansJP[wght].ttf` |
| Noto Sans KR | `NotoSansKR.ttf` | 10,414,588 | `NotoSansKR[wght].ttf` |

## Why it is not under `addons/`

A release packages every folder under `addons/`, and the addon files of one release are bounded at 64 MiB
(67,108,864 bytes). These four files are 49,718,756 bytes together, so with them the release was refused. The
package therefore lives outside `addons/` and is installed separately, only on nodes that print Chinese, Japanese
or Korean text.

It is a font directory, not a lifecycle addon: it has no `manifest.toml`, declares no models and no records, and
must never be placed on the addons path. The release packager refuses a folder under `addons/` without a manifest,
so a misplaced copy fails there instead of pushing a release over the bound.

## Installing it

1. Copy this directory to the node (the four `.ttf` files, `fonts.json`, `source-inventory.json`, `NOTICE` and
   `licenses/`). When the copy came without the font files, run `python3 tools/fetch-assets.py`: it downloads each
   absent file from its pinned URL and writes it only when the byte count and SHA-256 match `source-inventory.json`.
2. Check the bytes: `python3 tools/verify-assets.py`.
3. Give the directory to the renderer: `TypstEngine::new(default_fonts, limits)?.with_optional_fonts(OptionalFonts::Cjk, directory)?`
. On a server node, set `[report] cjk_font_directory` in the node
   configuration to this directory; the render workers receive it at start.

A report asks for a family by name (`DocumentOptions::with_fonts`). On a node without this package, a report that
names one of the four families fails with a font error that names the package; no other font is substituted.

## Trust

The renderer never reads `fonts.json` from this directory at run time. The file is compiled into the renderer, as
the default font manifest is, and a font file is parsed only when its bytes have the SHA-256 compiled in. A changed
or replaced file in this directory is refused as a digest mismatch. The directory adds no way to admit an uploaded
font.

## Provenance

All font bytes and each family's OFL notice come from `google/fonts` at the immutable commit
`cc39c4c2c304bc368c9b7c311ea2bcc6013299fe`. They are the same bytes that `addons/report_fonts` held until the four
families were taken out of it: the files here were restored from this repository's history, and their SHA-256
values equal the rows of the earlier `addons/report_fonts/source-inventory.json`. The rows of `fonts.json` and
`source-inventory.json` are those earlier rows, unchanged. The font bytes and OFL notices are unmodified; local
font filenames omit the bracketed axis suffix.

- `provenance/source-listings.json`: the GitHub contents API responses for the four source directories, copied from
  the earlier `addons/report_fonts/provenance/initial-source-listings.json`. No retrieval time was recorded with
  them and none is added here.
- `provenance/all-assets-verification.json`: raw tool-stamped output of `tools/verify-assets.py` on the files in
  this directory.
- `provenance/fetch-check.json`: raw tool-stamped output of one run of `tools/fetch-assets.py` into an empty
  temporary directory. The eight downloaded files were accepted by the tool's byte count and SHA-256 check, and
  the four font files were then compared byte for byte (`cmp`) with the files in this directory: equal.

These are the complete upstream variable-weight TTF files, without subsetting. The package does not assert that
every Unicode character, weight or language behaviour is supported. Follow the individual OFL notices when
distributing the font files; the project licence does not replace their licences.
