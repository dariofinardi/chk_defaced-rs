# Building `chk_defaced`

Everything the crate needs, what each feature costs, and where the native pieces come from.

The short version: **the default build is pure Rust and needs nothing** — no Tesseract to compile,
no pdfium to fetch, no GUI toolkit. Every native dependency sits behind an opt-in feature.

```bash
cargo build --release          # library + CLI, pure Rust
cargo test --lib               # 61 tests
```

---

## Feature matrix

| Feature | Default | What it adds | Native requirement |
|---|---|---|---|
| `cli` | yes | The `chk_defaced` binary (`clap`) | — |
| `html` | yes | HTML+CSS backend, static pass (`scraper`) | — |
| `ocr-tesseract` | no | Tesseract 5 backend (`tesseract5-rs`) | Tesseract + Leptonica, built by the binding |
| `ocr-specimen` | no | Specimen-OCR escalation: renders a glyph from its outline and reads it back (`tiny-skia`) | as `ocr-tesseract` |
| `ocr-atlas` | no | Render-level confirmation: rasterizes PDF pages and OCRs them (`pdfium-render`, `image`) | as `ocr-tesseract` **plus pdfium** |
| `render-wry` | no | DOCX rendered in a real webview, then screenshotted (`wry`, `tao`, `xcap`) | system webview |
| `render-ocr` | no | `render-wry` + `ocr-tesseract`: confirms DOCX collisions by rendering | both of the above |

Features compose: `ocr-atlas` and `ocr-specimen` both imply `ocr-tesseract`.

```bash
# deterministic scanning only — no OCR, no rendering
cargo build --release --no-default-features

# as a library inside another crate, without CLI and HTML
chk_defaced = { version = "0.3", default-features = false }

# the full PDF verification chain
cargo build --release --features ocr-atlas,ocr-specimen
```

---

## Native dependencies

### Tesseract (features `ocr-*`)

Provided by [`tesseract5-rs`](https://crates.io/crates/tesseract5-rs), which builds Tesseract and
Leptonica and installs them under a per-user directory — on Windows:

```
%APPDATA%\tesseract-rs\<arch>\static\{tesseract,leptonica}\lib      ~80 MB
%APPDATA%\tesseract-rs\<arch>\dynamic\tessdata\*.traineddata
```

Nothing else is required at build time: the crate does not link a system Tesseract.

### Language data (`*.traineddata`)

Tesseract reads nothing without the model for the language. Lookup order, in
`ocr::TesseractOcr::find_tessdata`:

1. `$TESSDATA_PREFIX`
2. `%APPDATA%\tesseract-rs\<arch>\dynamic\tessdata` — `<arch>` from `std::env::consts::ARCH`
3. `%APPDATA%\tesseract-rs\aarch64\dynamic\tessdata` — legacy fallback
4. `%APPDATA%\tesseract-rs\tessdata`
5. `C:\Program Files\Tesseract-OCR\tessdata`

A directory counts only if it actually contains `eng.traineddata`.

Models: [tesseract-ocr/tessdata](https://github.com/tesseract-ocr/tessdata) (Apache-2.0).
`tessdata_fast` is enough for the specimen path — it reads single glyphs and short words, not pages.

### pdfium (feature `ocr-atlas`)

Rasterizes PDF pages for the render-level confirmation. The dynamic library is **not** bundled:
point `PDFIUM_DIR` at the directory containing it (it defaults to the current directory).

Prebuilt binaries for every platform:
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries). pdfium itself is
BSD-3-Clause (Google).

### System webview (features `render-*`)

`wry`/`tao` embed the platform webview: WebView2 on Windows, WebKitGTK on Linux, WKWebView on
macOS. On Linux this pulls GTK/WebKitGTK development packages — the heaviest path in the crate, and
the reason it is off by default. (Upstream requirement, not verified here.)

---

## Environment variables

Every variable the crate actually reads:

| Variable | Used by | Meaning |
|---|---|---|
| `TESSDATA_PREFIX` | `ocr-*` | Directory holding `*.traineddata`. First candidate in the lookup above. |
| `PDFIUM_DIR` | `ocr-atlas` | Directory containing the pdfium dynamic library. Defaults to `.`. |
| `APPDATA`, `USERPROFILE` | `ocr-*` | Windows fallbacks for the tessdata lookup. |
| `WINDIR` | CLI | Default system-font directory for `build-registry` (`%WINDIR%\Fonts`). |

There is no build-time download and no network access at scan time: **nothing is fetched while
compiling**. That keeps builds hermetic and reproducible — a property that matters more than usual
for a tool whose job is verifying integrity.

---

## Platforms

Developed and verified on **Windows on ARM64** (MSVC). The default build is pure Rust and portable;
the native features depend on what the upstream projects support.

| | default build | `ocr-*` | `render-*` |
|---|---|---|---|
| Windows arm64 | verified | verified | verified |
| Windows x64 | expected | expected | expected |
| Linux | verified in CI | untested | needs GTK/WebKitGTK |
| macOS | expected | untested | untested |

"Verified" means measured here; "expected" means it follows from the dependencies without having
been run.

---

## Verification

What CI runs, and what you can run locally. The blocking job uses a **pinned toolchain** (1.98.0):
with `-D warnings` a new clippy release could otherwise break the build without a line of code
changing. A second job runs on `stable` and **warns without blocking**, so new lints surface when
they appear rather than when the pin is raised.

```bash
cargo build --all-targets
cargo test --lib
cargo test --doc
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
cargo build --no-default-features
```

The pinned number is a *tested with*. The **MSRV is declared separately** as `rust-version = "1.88"`,
and a third CI job builds and tests with exactly that toolchain — a declared MSRV nobody checks is a
promise that quietly ages. 1.88 is not a preference: with 1.86 cargo refuses the tree, because
`lopdf 0.44` requires it (along with `time`, `time-core` and `weezl`).

Some tests read fixtures from a corpus that is not in this repository; they skip themselves when it
is absent, so the suite is green either way.

---

## Dependency inventory

### Always present — all pure Rust

| Crate | License | Role |
|---|---|---|
| `skrifa` | MIT OR Apache-2.0 | Embedded fonts and glyph outlines — **the core of the check** |
| `lopdf` | MIT | PDF parsing: objects, `ToUnicode`, content streams |
| `zip` | MIT | DOCX container, `.odttf` de-obfuscation |
| `quick-xml` | MIT | OOXML and XMP metadata |
| `sha2` | MIT OR Apache-2.0 | Outline hashing for the registry |
| `unicode-security` | MIT/Apache-2.0 | TR39 confusables |
| `unicode-script` | MIT OR Apache-2.0 | Script of a character (cross-script sharing is legitimate) |
| `unicode-normalization` | MIT OR Apache-2.0 | NFKD — compatibility equivalence and ligatures |
| `deunicode` | BSD-3-Clause | ASCII base of a character |
| `serde`, `serde_json` | MIT OR Apache-2.0 | JSON report |
| `anyhow` | MIT OR Apache-2.0 | Error handling |
| `regex` | MIT OR Apache-2.0 | `ToUnicode` and content-stream parsing |
| `walkdir` | Unlicense/MIT | Directory traversal |

### Optional

| Crate | License | Feature |
|---|---|---|
| `clap` | MIT OR Apache-2.0 | `cli` |
| `scraper` | ISC | `html` |
| `tesseract5-rs` | MIT | `ocr-tesseract` |
| `tiny-skia` | BSD-3-Clause | `ocr-specimen` |
| `pdfium-render`, `image` | MIT OR Apache-2.0 | `ocr-atlas` |
| `wry` | Apache-2.0 OR MIT | `render-wry` |
| `tao` | Apache-2.0 | `render-wry` |
| `xcap` | Apache-2.0 | `render-wry` |
| `raw-window-handle` | MIT OR Apache-2.0 OR Zlib | `render-wry` |
| `windows` | MIT OR Apache-2.0 | `render-wry` |

The crate itself is **AGPL-3.0-only**; every dependency above is permissive, so none of them
constrains how you use it.

---

## `cargo audit`

Running it on the lockfile reports advisories. Which ones matter depends on **whether they are in
the default graph**:

| Advisory | In the default build? |
|---|---|
| `RUSTSEC-2026-0194` / `-0195` — `quick-xml` 0.30 | **No.** Build-dependency of `xcb`, pulled by `xcap` under `render-wry`, on X11 targets only. It runs at compile time to read X11 protocol descriptions and never sees a document. |
| `RUSTSEC-2026-0258` — `h2` | **No.** Optional GUI path. |
| `RUSTSEC-2024-0413` / `-0416` — `atk`, `atk-sys` unmaintained | **No.** GTK3 bindings via `wry` on Linux. |
| `RUSTSEC-2025-0057` — `fxhash` unmaintained | **Yes**, and *knowingly kept for now*. It arrives through `scraper` → `selectors`, i.e. from the `html` feature, which is on by default — not from a direct choice of ours. It is abandonment, not a vulnerability. Two ways out, both for **0.5**: drop the HTML backend from the default features, or move it to a maintained selector engine. Until then, `default-features = false` removes it for anyone who does not need the HTML pass. |

The direct `quick-xml` is 0.41, the patched version. `ttf-parser` (RUSTSEC-2026-0192, unmaintained)
was the other advisory in the default graph until 0.4.0 replaced it with `skrifa`.

---

## Redistributing the native artifacts

If you mirror Tesseract, tessdata or pdfium (for example as GitHub release assets), their licenses
permit it — Apache-2.0, Apache-2.0 and BSD-3 respectively — provided the license texts travel with
the binaries.

Two things worth deciding consciously first:

- **Publish SHA-256 checksums and the upstream release each file came from**, and verify them after
  download. A release asset can be replaced and a tag can be moved; for a tool that verifies
  document integrity, trusting a URL is the wrong posture.
- **A mirror ages.** When upstream ships a security fix, your copy serves the vulnerable build until
  you refresh it. That is a standing maintenance commitment, not a one-off upload.

Downloading inside `build.rs` is deliberately *not* done: it would break docs.rs (which builds
without network) and hermetic or offline builds, and a build-time fetch is exactly the vector an
integrity checker should help detect. If automatic provisioning is ever wanted, the right shape is
an explicit opt-in command that downloads at **runtime** into a cache and verifies checksums pinned
in the source.
