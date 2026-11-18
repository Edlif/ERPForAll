<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Report font assets

This data-only addon packages Noto and Amiri font files for an embedding report renderer. It declares no models or database records. Its empty data list does not install a font registry: the embedding caller loads the manifest and admitted bytes explicitly.

All font bytes and each family's OFL notice come from `google/fonts` at the immutable commit `cc39c4c2c304bc368c9b7c311ea2bcc6013299fe`. The source directory listing was retrieved with the GitHub contents API at that commit. Files were downloaded from the pinned raw URLs recorded in `fonts.json` and `source-inventory.json`; no system-font installation was used. Downloaded byte lengths were compared to the API listing, each font was checked against a 20 MiB maximum, and SHA-256 values were calculated from the saved download bytes. The original font bytes and OFL notices are unmodified; local font filenames omit bracketed axis suffixes.

`fonts.json` is an array of `{family, file, sha256, source, license}` entries. Paths are relative to this directory. A family can have multiple entries: Noto Sans includes its upright and italic files. `source-inventory.json` additionally records actual byte sizes, original upstream filenames and the licence-file hashes/URLs. Hashes identify these assets; they do not prove rendering support or confer trust on user uploads.

| Family | Included font files | Original axis suffix |
|---|---|---|
| Noto Sans | `NotoSans.ttf`, `NotoSans-Italic.ttf` | `[wdth,wght]` |
| Amiri | `Amiri-Regular.ttf` | None (static Regular face) |
| Noto Naskh Arabic | `NotoNaskhArabic.ttf` | `[wght]` |
| Noto Sans Arabic | `NotoSansArabic.ttf` | `[wdth,wght]` |
| Noto Sans Hebrew | `NotoSansHebrew.ttf` | `[wdth,wght]` |
| Noto Sans Thai | `NotoSansThai.ttf` | `[wdth,wght]` |
| Noto Sans Devanagari | `NotoSansDevanagari.ttf` | `[wdth,wght]` |

These are the complete selected upstream TTF files, without subsetting or replacement families. The package does not assert that all Unicode characters, weights, styles or language behavior are supported. Additional script families and a general-purpose Latin serif family are not included. The renderer must declare and validate fallback choices rather than scanning host fonts.

Font parser validation, actual face metadata inspection, addon-install validation, PDF rendering, text extraction and shaping/raster review have **not been run** for this package. In particular, file naming and upstream family labels are provenance, not a claim that the renderer has parsed these faces. Follow the individual OFL notices when distributing font files; the project licence does not replace their licences.

Noto Naskh Arabic is an additional candidate family; Noto Sans Arabic remains packaged. Its acquisition script and tool-stamped raw output are kept in the project's acquisition records, which are not part of the public source tree. No parser, shaping or text-extraction result is claimed for this candidate.

A current full saved-byte verification is reproducible with `python3 addons/report_fonts/tools/verify-assets.py`. Its raw tool-stamped output, the original acquisition script and the source API responses are kept in the same acquisition records. The initial script has its original temporary-input/workspace paths and records no historical execution timestamp; the current verification supplies fresh provenance without inventing one. These are acquisition/integrity records, not rendering tests.

Amiri Regular is an additional Arabic candidate from the same pinned source, without replacing existing assets. Only its Regular face is packaged; Bold, Italic and BoldItalic are not included. Its acquisition evidence is kept in the same acquisition records. No shaping or extraction fix is claimed.

The Chinese, Japanese and Korean families (Noto Sans SC, TC, JP, KR; 49,718,756 bytes) are not shipped here: the
addon files of one release are bounded at 64 MiB. They are the optional package `optional-assets/report_fonts_cjk/`,
installed separately on the nodes that need them. A report
that asks for one of them on a node without the package gets a font error that names the package.
