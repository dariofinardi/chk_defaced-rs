//! `chk_defaced` — library API: font-coherence checks for documents (PDF/DOCX/HTML) and a
//! system-font cmap registry. Used both by the `chk_defaced` CLI and as a dependency (e.g. to run a
//! pre-extraction coherence check before ingesting a document).
//!
//! Background: "What you see is not what your AI reads"
//! <https://dariofinardi.it/what-you-see-is-not-what-your-ai-reads-c3fed388d3bc>.
//!
//! Author: **Dario Finardi**. Developed as part of the document-integrity work for
//! **Edito** (<https://edito-pdf.com>), the GDPR-native document intelligence platform by
//! **Jugaad s.r.l.** (Italy). Released independently under AGPL-3.0-only.
//!
//! Minimal embedding example:
//! ```no_run
//! let report = chk_defaced::scan::scan_path(std::path::Path::new("contract.pdf"), None)?;
//! if report.findings.iter().any(|f| f.severity >= chk_defaced::finding::Severity::High) {
//!     eprintln!("document may be defaced: extracted text could diverge from what is rendered");
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! Se il PDF e' **gia' caricato** — una pipeline che lo apre per conto suo non deve farlo
//! parsare due volte — l'ingresso e' [`scan::scan_document`], che prende il `lopdf::Document`
//! esistente. Vale finche' chiamante e crate concordano sulla versione di `lopdf`:
//! ```no_run
//! let doc = lopdf::Document::load("contract.pdf")?;
//! let report = chk_defaced::scan::scan_document(&doc, "contract.pdf", None)?;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! The `html` feature pulls the (heavier) HTML+CSS backend; disable it (`default-features = false`)
//! when only PDF/DOCX scanning is needed.
//!
//! # Feature-gated modules — not visible here
//!
//! docs.rs builds the **default** features only, so four modules are missing from this page even
//! though they exist. They are the OCR and rendering half of the crate:
//!
//! | Module | Feature | What it does |
//! |---|---|---|
//! | `specimen` | `ocr-specimen` | Renders a glyph straight from its outline and reads it back, to confirm or **refute** a deterministic finding without rasterising a page |
//! | `atlas` | `ocr-atlas` | Renders the PDF pages and compares the read text with the extracted one |
//! | `render` | `render-wry` | Renders a DOCX in a real webview — the positional attack a global char→char map cannot represent |
//! | `glyph` | `ocr-atlas` | Glyph-level helpers shared by those paths |
//!
//! Build them with e.g. `--features ocr-atlas,ocr-specimen`; they need Tesseract and (for `atlas`)
//! pdfium. What each one costs, where the artefacts come from and which environment variables are
//! read is in [BUILD.md](https://github.com/dariofinardi/chk_defaced-rs/blob/main/BUILD.md).

/// One embedded font for the specimen-OCR escalation: its raw bytes and the `(claimed_char, glyph_id)`
/// pairs the document extracts. Produced by `pdf_glyph::pdf_font_claims` / `docx_glyph::docx_font_claims`,
/// consumed by `specimen::specimen_scan`.
pub type FontClaims = (Vec<u8>, Vec<(char, u16)>);

#[cfg(feature = "ocr-atlas")]
pub mod atlas;
pub mod docx_glyph;
pub mod docx_html;
pub mod finding;
pub mod pdf_glyph;
pub mod font;
pub mod glyphmatch;
pub mod metadata;
#[cfg(feature = "ocr-atlas")]
pub mod glyph;
pub mod ocr;
pub mod registry;
#[cfg(feature = "render-wry")]
pub mod render;
pub mod scan;
#[cfg(feature = "ocr-specimen")]
pub mod specimen;
pub mod textdiff;
pub mod unicode;
pub mod visibility;
