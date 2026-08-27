//! Specimen-OCR escalation — the answer to the one case the deterministic outline cross-reference
//! cannot decide: a **fully custom font with no honest anchor**. When every code is remapped 1:1 and the
//! true letter never appears un-tampered anywhere in the document, no internal outline collision forms,
//! so [`crate::pdf_glyph`] / [`crate::docx_glyph`] stay silent. There is then no in-document ground
//! truth to compare against — so we manufacture one.
//!
//! For each distinct embedded glyph we render a **specimen** (the glyph itself, repeated on a line,
//! rasterized straight from its outline — no page, no pdfium) and OCR it. OCR recovers *what the glyph
//! actually draws*, independently of the font's lying cmap. If that disagrees with the character the
//! document claims to extract for that glyph, the font draws one letter and reports another: semantic
//! replacement, caught with **no honest anchor required**.
//!
//! Cost scales with the number of *distinct* glyphs, not the page count: identical outlines are OCR'd
//! once. Conservative by construction — an inconclusive/low-agreement OCR read never raises a finding.
//!
//! **Precision profile (be honest about it).** This is an *escalation*, not the default path. Recall is
//! high — it catches the no-anchor case the outline cross-reference structurally cannot (validated: 5/5
//! planted lies with zero honest mappings). Two filters keep precision in check:
//! - **ligatures excluded** — a ligature codepoint draws several letters, so single-letter OCR can never
//!   match it; comparing one is a guaranteed false positive.
//! - **confidence gate** ([`MIN_CONF`]) — genuine drawn letters OCR at ≥80 while the look-alike
//!   confusions (`c/e`, `s/f`, `ı/l`, `a/d`) land ≤47, so a gate in that gap drops them.
//!
//! Together these took a real LaTeX paper from 8 false positives to **0**, with the deterministic true
//! positives unchanged. The residual cost is *recall*: a glyph whose isolated shape OCRs below the gate
//! (e.g. a double-story `g`) is skipped rather than guessed. Net: trustworthy enough to surface, but
//! still an escalation for the cases the deterministic detectors can't decide — read findings as strong
//! candidates, not proof.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::finding::{Category, Finding, Report, Severity};
use crate::ocr::{GrayImage, OcrEngine, OcrHint};

const PX: f32 = 72.0; // glyph height in pixels (bigger reads more reliably)
const PAD: f32 = 12.0; // border around the specimen line
const GAP: f32 = 16.0; // spacing between repeated glyphs
const REPEATS: usize = 6; // how many copies of the glyph to put on the specimen line
// Discard OCR reads below this confidence (Tesseract 0–100). Chosen from the measured gap: genuine
// drawn letters score ≥80, while the cross-letter OCR confusions (c/e, s/f, ı/l, a/d) score ≤47.
const MIN_CONF: f32 = 65.0;

/// A typographic ligature codepoint (Alphabetic Presentation Forms: ﬀ ﬁ ﬂ ﬃ ﬄ ﬅ ﬆ …). Its glyph
/// legitimately draws *several* letters, so single-letter OCR can never agree with it — comparing one
/// would be a guaranteed false positive. These are normal typesetting, not semantic replacement, so we
/// never raise a finding for a claimed-ligature character.
fn is_ligature(c: char) -> bool {
    matches!(c as u32, 0xFB00..=0xFB4F)
}

/// Feeds a `ttf_parser` glyph outline into a `tiny_skia` path, mapping font units (Y-up) to image
/// pixels (Y-down) with the glyph's bounding box placed at `(PAD, PAD)`.
struct PathPen {
    pb: tiny_skia::PathBuilder,
    scale: f32,
    x_min: f32,
    y_max: f32,
}
impl PathPen {
    fn ix(&self, x: f32) -> f32 {
        (x - self.x_min) * self.scale + PAD
    }
    fn iy(&self, y: f32) -> f32 {
        (self.y_max - y) * self.scale + PAD
    }
}
impl ttf_parser::OutlineBuilder for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(self.ix(x), self.iy(y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(self.ix(x), self.iy(y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(self.ix(x1), self.iy(y1), self.ix(x), self.iy(y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(self.ix(x1), self.iy(y1), self.ix(x2), self.iy(y2), self.ix(x), self.iy(y));
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

/// Rasterize one glyph, repeated `REPEATS` times on a line, to a grayscale specimen image.
/// Returns `None` for empty/space glyphs (nothing to OCR).
fn render_specimen(face: &ttf_parser::Face, gid: u16) -> Option<GrayImage> {
    render_specimen_px(face, gid, PX)
}

/// [`render_specimen`] a una risoluzione data. Il contorno (`PAD`) e la spaziatura (`GAP`) scalano
/// con l'altezza, cosi' il ritaglio resta **stretto sul glifo** a ogni risoluzione: piu' pixel sulla
/// lettera, non piu' bianco intorno.
pub(crate) fn render_specimen_px(face: &ttf_parser::Face, gid: u16, px: f32) -> Option<GrayImage> {
    let bbox = face.glyph_bounding_box(ttf_parser::GlyphId(gid))?;
    let upem = face.units_per_em() as f32;
    if upem <= 0.0 {
        return None;
    }
    let scale = px / upem;
    let (pad, gap) = (PAD * px / PX, GAP * px / PX);
    let gw = (bbox.x_max - bbox.x_min) as f32 * scale;
    let gh = (bbox.y_max - bbox.y_min) as f32 * scale;
    if gw < 1.0 || gh < 1.0 {
        return None;
    }

    let mut pen = PathPen {
        pb: tiny_skia::PathBuilder::new(),
        scale,
        x_min: bbox.x_min as f32,
        y_max: bbox.y_max as f32,
    };
    face.outline_glyph(ttf_parser::GlyphId(gid), &mut pen)?;
    let path = pen.pb.finish()?;

    let step = gw + gap;
    let w = (pad * 2.0 + gw + (REPEATS as f32 - 1.0) * step).ceil() as u32;
    let h = (gh + pad * 2.0).ceil() as u32;
    let mut pm = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
    pm.fill(tiny_skia::Color::WHITE);

    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::BLACK);
    paint.anti_alias = true;
    for k in 0..REPEATS {
        let dx = k as f32 * step;
        pm.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::from_translate(dx, 0.0),
            None,
        );
    }

    // RGBA8 (premultiplied, black-on-white) → 1-byte luma. The red channel suffices for grayscale ink.
    let gray: Vec<u8> = pm.data().chunks_exact(4).map(|px| px[0]).collect();
    Some(GrayImage::new(w, h, gray))
}

/// The single letter the OCR engine read from a specimen line, if it read one *confidently*. Tesseract
/// often groups the repeated glyphs into one or a few words, so we don't count instances against
/// `REPEATS`; instead we keep only reads at or above [`MIN_CONF`] (dropping the low-confidence `c/e`,
/// `s/f`, `ı/l`, `a/d` confusions) and require the dominant letter to be a **strict majority** of those
/// confident reads. No confident read, or a tie, returns `None` → no finding.
fn vote(scored: &[(char, f32)]) -> Option<char> {
    let mut counts: HashMap<char, usize> = HashMap::new();
    let mut total = 0usize;
    for &(c, conf) in scored {
        if conf < MIN_CONF || !c.is_alphabetic() {
            continue;
        }
        if let Some(l) = c.to_lowercase().next() {
            *counts.entry(l).or_default() += 1;
            total += 1;
        }
    }
    let (top, n) = counts.into_iter().max_by_key(|&(_, n)| n)?;
    (n * 2 > total).then_some(top) // strict majority among the confident reads
}

/// Rende una **parola** con i glifi indicati, rispettando le avanzate orizzontali del font.
///
/// A differenza dello specimen a glifo isolato — sei copie della stessa lettera, che per Tesseract
/// e' una non-parola e lo lascia senza aiuto dal modello linguistico — qui l'immagine e' una parola
/// reale del documento, disegnata dai glifi che il documento usa davvero. Quindi mostra **cio' che
/// il lettore vede**, senza rasterizzare pagine e senza pdfium.
pub(crate) fn render_word_px(face: &ttf_parser::Face, gids: &[u16], px: f32) -> Option<GrayImage> {
    let upem = face.units_per_em() as f32;
    if upem <= 0.0 || gids.is_empty() {
        return None;
    }
    let scale = px / upem;
    let pad = PAD * px / PX;

    // Estensione verticale della parola intera (non del singolo glifo): ascendenti e discendenti
    // devono starci, altrimenti il ritaglio taglia le lettere e l'OCR peggiora.
    let (mut y_min, mut y_max) = (f32::MAX, f32::MIN);
    let mut larghezza = 0.0f32;
    for &g in gids {
        larghezza += face.glyph_hor_advance(ttf_parser::GlyphId(g)).unwrap_or(0) as f32;
        if let Some(b) = face.glyph_bounding_box(ttf_parser::GlyphId(g)) {
            y_min = y_min.min(b.y_min as f32);
            y_max = y_max.max(b.y_max as f32);
        }
    }
    if larghezza <= 0.0 || y_max <= y_min {
        return None;
    }
    let w = (larghezza * scale + pad * 2.0).ceil() as u32;
    let h = ((y_max - y_min) * scale + pad * 2.0).ceil() as u32;
    let mut pm = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
    pm.fill(tiny_skia::Color::WHITE);
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::BLACK);
    paint.anti_alias = true;

    let mut x = 0.0f32;
    for &g in gids {
        let mut pen = PathPen { pb: tiny_skia::PathBuilder::new(), scale, x_min: 0.0, y_max };
        if face.outline_glyph(ttf_parser::GlyphId(g), &mut pen).is_some() {
            if let Some(path) = pen.pb.finish() {
                pm.fill_path(
                    &path,
                    &paint,
                    tiny_skia::FillRule::Winding,
                    tiny_skia::Transform::from_translate(pad + x * scale, pad),
                    None,
                );
            }
        }
        x += face.glyph_hor_advance(ttf_parser::GlyphId(g)).unwrap_or(0) as f32;
    }
    let gray: Vec<u8> = pm.data().chunks_exact(4).map(|px| px[0]).collect();
    Some(GrayImage::new(w, h, gray))
}

/// Da che parte sta una **parola** del documento, resa con i glifi del font e riletta.
///
/// Confronta due ipotesi complete invece di due lettere nude: la parola come l'estrattore l'ha letta,
/// e la stessa parola con le sostituzioni applicate. Se il font mente, l'immagine mostra la seconda e
/// Tesseract la legge; se e' onesto, mostra la prima. Il verdetto usa lo stesso
/// [`crate::textdiff::phrase_side`] della verifica a render, quindi le due prove parlano la stessa
/// lingua.
///
/// `None` quando la parola non e' utilizzabile: glifo mancante per qualche carattere, oppure nessuna
/// sostituzione che la cambi (senza differenza non c'e' niente da decidere).
///
/// ⚠️ **Limite del chiamante, misurato.** `char_to_gid` associa un solo glifo per carattere, ma un
/// defacement e' esattamente il caso in cui **lo stesso carattere ha due glifi**: quello onesto, usato
/// quasi ovunque, e quello bugiardo, usato in pochi punti. Con una mappa a un glifo si rende quasi
/// sempre l'onesto e la parola torna corretta, quindi un attacco *acceso* resta invisibile. Sulla
/// fixture `replaced.pdf` 23 parole su 27 sono risultate a favore del testo estratto **sia** con la
/// mappa diretta sia con l'inversa: non e' una prova che il font sia onesto, e' la sonda che guarda
/// nel posto sbagliato. Per vedere l'attacco servono i **gid effettivamente usati in quella
/// posizione**, che stanno nel content stream, non nel riassunto ToUnicode.
pub fn word_specimen_side(
    face: &ttf_parser::Face,
    char_to_gid: &HashMap<char, u16>,
    word: &str,
    subs: &[(char, char)],
    ocr: &dyn OcrEngine,
) -> Option<crate::textdiff::PhraseSide> {
    let presumed = crate::finding::apply_substitutions(word, subs);
    if presumed == word {
        return None;
    }
    let gids: Option<Vec<u16>> = word
        .chars()
        .map(|c| {
            char_to_gid
                .get(&c)
                .or_else(|| char_to_gid.get(&c.to_lowercase().next().unwrap_or(c)))
                .copied()
        })
        .collect();
    let img = render_word_px(face, &gids?, PX)?;
    let letto = ocr.recognize(&img, OcrHint::SingleLine).ok()?;
    if letto.trim().is_empty() {
        return None;
    }
    Some(crate::textdiff::phrase_side(word, &presumed, &letto))
}

/// Run the specimen-OCR check over a set of embedded fonts, each given as its raw bytes plus the
/// `(claimed_char, glyph_id)` pairs the document extracts. `rule`/`label` tag the emitted findings.
///
/// For every distinct glyph: render → OCR → if the OCR letter disagrees (non-homoglyph) with a claimed
/// character, the font draws one letter and reports another.
/// Ciò che lo specimen-OCR legge davvero su ogni glifo: coppie `(carattere_estratto, lettera_letta)`,
/// **accordi inclusi**.
///
/// Gli accordi sono la novità: prima venivano scartati sul posto, e con essi l'unica prova capace di
/// scagionare un finding deterministico. Vedi [`crate::glyphmatch::specimen_verdict`].
pub fn specimen_reads(fonts: &[crate::FontClaims], ocr: &dyn OcrEngine) -> Vec<(char, char)> {
    let mut out = Vec::new();
    for (bytes, claims) in fonts {
        let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue };
        let mut by_gid: HashMap<u16, Vec<char>> = HashMap::new();
        for &(ch, gid) in claims {
            if ch.is_alphabetic() && !is_ligature(ch) {
                by_gid.entry(gid).or_default().push(ch);
            }
        }
        for (gid, claimed) in by_gid {
            let Some(img) = render_specimen(&face, gid) else { continue };
            let scored: Vec<(char, f32)> = ocr
                .recognize_scored(&img, OcrHint::SingleLine)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(w, conf)| w.chars().find(|c| c.is_alphabetic()).map(|c| (c, conf)))
                .collect();
            let Some(drawn) = vote(&scored) else { continue };
            for ch in claimed {
                out.push((ch.to_lowercase().next().unwrap_or(ch), drawn));
            }
        }
    }
    out
}

pub fn specimen_scan(
    fonts: &[crate::FontClaims],
    ocr: &dyn OcrEngine,
    rule: &str,
    label: &str,
) -> Result<Vec<Finding>> {
    let mut lies: HashMap<(char, char), usize> = HashMap::new();

    for (bytes, claims) in fonts {
        let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue };
        // distinct glyph → the claimed letters that reach it
        let mut by_gid: HashMap<u16, Vec<char>> = HashMap::new();
        for &(ch, gid) in claims {
            // Single base letters only: ligatures draw several letters (OCR can't match → false positive).
            if ch.is_alphabetic() && !is_ligature(ch) {
                by_gid.entry(gid).or_default().push(ch);
            }
        }
        for (gid, claimed) in by_gid {
            let Some(img) = render_specimen(&face, gid) else { continue };
            // Per-instance (letter, confidence); confidence-gated majority vote rejects shaky reads.
            let scored: Vec<(char, f32)> = ocr
                .recognize_scored(&img, OcrHint::SingleLine)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(w, conf)| w.chars().find(|c| c.is_alphabetic()).map(|c| (c, conf)))
                .collect();
            let Some(drawn) = vote(&scored) else { continue }; // inconclusive/low-confidence → no finding
            for ch in claimed {
                if !crate::glyphmatch::legitimately_identical_ci(drawn, ch) {
                    // OCR sees `drawn`; the document extracts `ch` → the font claims a different letter.
                    let extracted = ch.to_lowercase().next().unwrap_or(ch);
                    *lies.entry((drawn, extracted)).or_default() += 1;
                }
            }
        }
    }

    let mut findings: Vec<Finding> = lies
        .into_iter()
        .map(|((drawn, extracted), n)| {
            Finding::new(
                rule,
                Severity::High,
                Category::FontIntegrity,
                label,
                format!(
                    "a glyph that OCRs as '{drawn}' is extracted as '{extracted}' ({n}×): the embedded font draws a different letter than it reports (semantic replacement confirmed by specimen OCR)"
                ),
                0.8,
            )
        })
        .collect();
    findings.sort_by(|a, b| a.message.cmp(&b.message));
    Ok(findings)
}

/// **Conferma** di finding deterministici già emessi, con lo specimen-OCR.
///
/// Speculare a [`specimen_scan_path`], che copre il caso opposto (nessun segnale deterministico, font
/// senza àncora onesta). Qui il segnale c'è già, e la domanda è se regga: si rende ogni glifo e lo si
/// legge, poi [`crate::glyphmatch::specimen_verdict`] confronta le letture con le sostituzioni
/// recuperate. Se il glifo disegna la lettera **estratta**, la `ToUnicode` era onesta e l'aggancio
/// dell'outline aveva preso un abbaglio: i finding scendono a `Info` e il verdetto diventa `Refuted`.
///
/// Costo: proporzionale ai **glifi distinti**, non alle pagine — niente rasterizzazione di pagine,
/// nessun pdfium. È la ragione per cui può girare come conferma ordinaria e non solo come escalation.
///
/// Non tocca nulla se le prove mancano o si contraddicono: il dubbio resta dov'era.
pub fn confirm_with_specimen(
    path: &std::path::Path,
    report: &mut Report,
    ocr: &dyn OcrEngine,
) -> Result<crate::glyphmatch::SpecimenVerdict> {
    use crate::glyphmatch::SpecimenVerdict;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let (subs, fonts) = match ext.as_str() {
        "pdf" => {
            let scan = crate::pdf_glyph::pdf_outline_scan(path).context("PDF outline scan")?;
            (scan.substitutions, crate::pdf_glyph::pdf_font_claims(path)?)
        }
        "docx" => {
            let scan = crate::docx_glyph::docx_outline_scan(path).context("DOCX outline scan")?;
            (scan.substitutions, crate::docx_glyph::docx_font_claims(path)?)
        }
        other => anyhow::bail!("specimen confirmation unsupported for .{other} (pdf, docx only)"),
    };
    if subs.is_empty() {
        return Ok(SpecimenVerdict::Inconclusive);
    }
    let reads = specimen_reads(&fonts, ocr);
    let verdict = crate::glyphmatch::specimen_verdict(&subs, &reads);
    match verdict {
        SpecimenVerdict::Refuted => {
            report.verdict = Some(crate::finding::Verdict::Refuted);
            for f in
                report.findings.iter_mut().filter(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT"))
            {
                f.severity = Severity::Info;
                f.message.push_str(
                    " — specimen-refuted: rendering the glyph and reading it back returns the extracted letter, so the font's mapping is honest and the outline match was wrong",
                );
            }
        }
        SpecimenVerdict::Corroborated => {
            report.push(Finding::new(
                "GLYPH_REPLACEMENT_SPECIMEN_CONFIRMED",
                Severity::High,
                Category::FontIntegrity,
                "embedded font",
                "specimen OCR confirms the glyph draws the letter the outline match attributes to it"
                    .to_string(),
                0.9,
            ));
        }
        SpecimenVerdict::Inconclusive => {}
    }
    Ok(verdict)
}

/// Convenience: scan a document's fonts by path, dispatching on extension. Builds the claim sets via the
/// same extraction the deterministic detectors use, then runs [`specimen_scan`].
pub fn specimen_scan_path(path: &std::path::Path, ocr: &dyn OcrEngine) -> Result<Vec<Finding>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let label = "embedded font";
    match ext.as_str() {
        "pdf" => {
            let fonts = crate::pdf_glyph::pdf_font_claims(path).context("extracting PDF font claims")?;
            specimen_scan(&fonts, ocr, "PDF.GLYPH_SEMANTIC_REPLACEMENT_OCR", label)
        }
        "docx" => {
            let fonts =
                crate::docx_glyph::docx_font_claims(path).context("extracting DOCX font claims")?;
            specimen_scan(&fonts, ocr, "DOCX.GLYPH_SEMANTIC_REPLACEMENT_OCR", label)
        }
        other => anyhow::bail!("specimen scan unsupported for .{other} (pdf, docx only)"),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_ligature, vote, MIN_CONF};
    use crate::glyphmatch::legitimately_identical_ci as legitimately_identical;

    #[test]
    fn ligatures_are_excluded() {
        assert!(is_ligature('\u{FB00}')); // ﬀ
        assert!(is_ligature('\u{FB03}')); // ﬃ
        assert!(is_ligature('\u{FB04}')); // ﬄ
        assert!(!is_ligature('a'));
        assert!(!is_ligature('\u{00E9}')); // é — a normal accented letter, not a ligature
    }

    #[test]
    fn homoglyphs_and_real_swaps() {
        assert!(legitimately_identical('a', 'a'));
        assert!(legitimately_identical('M', 'm')); // case only
        assert!(legitimately_identical('i', 'l')); // common single-glyph pair
        assert!(legitimately_identical('a', '\u{0430}')); // Latin a vs Cyrillic а (different script)
        assert!(legitimately_identical('b', '\u{03B2}')); // Latin b vs Greek β (cross-script sharing)
        assert!(legitimately_identical('z', '\u{03B6}')); // Latin z vs Greek ζ
        assert!(legitimately_identical('\u{00F0}', '\u{0111}')); // ð vs đ — same ASCII base "d"
        assert!(legitimately_identical('\u{00F0}', '\u{0256}')); // ð vs ɖ — same ASCII base "d"
        assert!(!legitimately_identical('m', 'd')); // a genuine semantic swap (same script)
        assert!(!legitimately_identical('r', 'n'));
    }

    #[test]
    fn vote_needs_confident_strict_majority() {
        let hi = MIN_CONF + 10.0;
        let lo = MIN_CONF - 10.0;
        assert_eq!(vote(&[('m', hi)]), Some('m')); // one confident read is enough
        assert_eq!(vote(&[]), None); // nothing read
        assert_eq!(vote(&[('c', lo), ('c', lo)]), None); // all below the confidence floor
        assert_eq!(vote(&[('m', hi), ('m', hi), ('x', hi)]), Some('m')); // confident majority
        assert_eq!(vote(&[('m', hi), ('d', hi)]), None); // tie → inconclusive
        assert_eq!(vote(&[('m', hi), ('d', lo)]), Some('m')); // the low-confidence rival is discarded
    }
}

#[cfg(test)]
mod confirm_tests {
    use super::*;
    use crate::finding::Verdict;
    use crate::ocr::MockOcr;
    use std::path::PathBuf;

    fn fixture() -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/attack-fixtures/replaced.pdf");
        p.exists().then_some(p)
    }

    /// La prima coppia `(estratto, disegnato)` della mappa recuperata. Ricavata a runtime e non
    /// scritta a mano: la direzione dipende dai dati, e darla per scontata rende il test una bugia.
    fn prima_coppia(p: &std::path::Path) -> (char, char) {
        let subs = crate::pdf_glyph::pdf_outline_scan(p).expect("scan").substitutions;
        *subs.first().expect("la fixture deve produrre sostituzioni")
    }

    fn scanned(path: &std::path::Path) -> crate::finding::Report {
        crate::scan::scan_path(path, None).expect("scan")
    }

    fn n_high_replacement(r: &crate::finding::Report) -> usize {
        r.findings
            .iter()
            .filter(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT") && f.severity == Severity::High)
            .count()
    }

    #[test]
    fn lo_specimen_scagiona_se_il_glifo_disegna_la_lettera_estratta() {
        let Some(p) = fixture() else { return };
        let (estratto, _) = prima_coppia(&p);
        let mut r = scanned(&p);
        assert!(n_high_replacement(&r) > 0, "la fixture deve partire con finding High");

        // L'OCR legge sul glifo la stessa lettera che il documento estrae: la mappatura e' onesta.
        let v = confirm_with_specimen(&p, &mut r, &MockOcr(estratto.to_string())).expect("conferma");
        assert_eq!(v, crate::glyphmatch::SpecimenVerdict::Refuted);
        assert_eq!(r.verdict, Some(Verdict::Refuted));
        assert_eq!(n_high_replacement(&r), 0, "i finding vanno declassati a Info");
    }

    #[test]
    fn lo_specimen_conferma_se_il_glifo_disegna_la_lettera_attribuita() {
        let Some(p) = fixture() else { return };
        let (_, disegnato) = prima_coppia(&p);
        let mut r = scanned(&p);
        let prima = n_high_replacement(&r);

        let v = confirm_with_specimen(&p, &mut r, &MockOcr(disegnato.to_string())).expect("conferma");
        assert_eq!(v, crate::glyphmatch::SpecimenVerdict::Corroborated);
        assert_eq!(n_high_replacement(&r), prima, "nessun declassamento");
        assert!(r.findings.iter().any(|f| f.rule.contains("SPECIMEN_CONFIRMED")));
    }

    #[test]
    fn letture_estranee_lasciano_il_dubbio_dov_era() {
        let Some(p) = fixture() else { return };
        let (estratto, disegnato) = prima_coppia(&p);
        // una lettera che non e' ne' l'estratto ne' l'attribuito
        let estranea = ('a'..='z').find(|c| *c != estratto && *c != disegnato).unwrap();
        let mut r = scanned(&p);
        let prima = n_high_replacement(&r);
        let verdetto_prima = r.verdict;

        let v = confirm_with_specimen(&p, &mut r, &MockOcr(estranea.to_string())).expect("conferma");
        assert_eq!(v, crate::glyphmatch::SpecimenVerdict::Inconclusive);
        assert_eq!(n_high_replacement(&r), prima, "il report non va toccato");
        assert_eq!(r.verdict, verdetto_prima, "il verdetto resta quello che era");
    }

    #[test]
    fn formati_non_supportati_danno_errore_esplicito() {
        let mut r = crate::finding::Report::new("x.html", "html");
        let err = confirm_with_specimen(std::path::Path::new("x.html"), &mut r, &MockOcr("a".into()))
            .unwrap_err();
        assert!(format!("{err:#}").contains("unsupported"), "{err:#}");
    }
}

#[cfg(test)]
mod resolution_experiment {
    use super::*;
    use std::path::PathBuf;

    /// Sceglie `PX` con una misura, non a intuito.
    ///
    /// Su un documento **pulito** il carattere dichiarato dal font e' la verita', quindi si puo'
    /// misurare l'accuratezza vera: si rende ogni glifo distinto a risoluzioni diverse, lo si legge
    /// con Tesseract e si conta quante volte torna la lettera giusta, con quanta confidenza.
    ///
    /// Ignorato di default (richiede tessdata reale):
    /// `cargo test --features ocr-specimen risoluzione -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn risoluzione_dello_specimen() {
        let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/fp-corpus/clean");
        // Il corpus pulito e' organizzato in sottocartelle (eu/us/misc): si scende di un livello.
        let mut pdfs: Vec<PathBuf> = Vec::new();
        let mut da_visitare = vec![corpus.clone()];
        while let Some(dir) = da_visitare.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    da_visitare.push(p);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
                    pdfs.push(p);
                }
            }
        }
        pdfs.sort();
        pdfs.truncate(6);
        if pdfs.is_empty() {
            eprintln!("skip: corpus pulito assente");
            return;
        }
        let tess = PathBuf::from(std::env::var("TESSDATA_PREFIX").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE").unwrap_or_default().replace(char::from(92), "/");
            format!("{home}/AppData/Roaming/tesseract-rs/aarch64/dynamic/tessdata")
        }));
        let ocr = match crate::ocr::TesseractOcr::new(Some(tess), "eng", OcrHint::SingleLine) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skip: Tesseract non inizializzabile ({e:#})");
                return;
            }
        };

        const RISOLUZIONI: [f32; 6] = [72.0, 96.0, 128.0, 160.0, 192.0, 256.0];
        let mut giuste = [0usize; 6];
        let mut lette = [0usize; 6];
        let mut conf_tot = [0f32; 6];
        let mut totale = 0usize;

        for pdf in &pdfs {
            let Ok(fonts) = crate::pdf_glyph::pdf_font_claims(pdf) else { continue };
            for (bytes, claims) in fonts.iter() {
                let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue };
                let mut visti = std::collections::HashSet::new();
                for &(ch, gid) in claims.iter() {
                    // Solo lettere latine di base: su un documento pulito il carattere dichiarato
                    // e' la verita' attesa. Un glifo per gid, al massimo 12 per font.
                    if !ch.is_ascii_alphabetic() || !visti.insert(gid) || visti.len() > 12 {
                        continue;
                    }
                    totale += 1;
                    for (k, px) in RISOLUZIONI.iter().enumerate() {
                        let Some(img) = render_specimen_px(&face, gid, *px) else { continue };
                        let scored = ocr.recognize_scored(&img, OcrHint::SingleLine).unwrap_or_default();
                        let letto = scored
                            .iter()
                            .filter_map(|(w, c)| w.chars().find(|c| c.is_alphabetic()).map(|l| (l, *c)))
                            .next();
                        if let Some((l, c)) = letto {
                            lette[k] += 1;
                            conf_tot[k] += c;
                            if crate::glyphmatch::legitimately_identical_ci(l, ch) {
                                giuste[k] += 1;
                            }
                        }
                    }
                }
            }
        }

        println!("
{} glifi distinti da {} PDF puliti
", totale, pdfs.len());
        println!("{:>6} {:>10} {:>12} {:>14}", "px", "accuratezza", "letti", "confidenza media");
        for (k, px) in RISOLUZIONI.iter().enumerate() {
            let acc = if totale > 0 { giuste[k] as f32 / totale as f32 * 100.0 } else { 0.0 };
            let cm = if lette[k] > 0 { conf_tot[k] / lette[k] as f32 } else { 0.0 };
            println!("{px:>6.0} {acc:>9.1}% {:>12} {cm:>14.1}", lette[k]);
        }
    }
}

#[cfg(test)]
mod word_context_experiment {
    use super::*;
    use std::path::PathBuf;

    fn tessdata() -> PathBuf {
        PathBuf::from(std::env::var("TESSDATA_PREFIX").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE").unwrap_or_default().replace(char::from(92), "/");
            format!("{home}/AppData/Roaming/tesseract-rs/aarch64/dynamic/tessdata")
        }))
    }

    fn pdfs_in(dir: &std::path::Path, max: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
                    out.push(p);
                }
            }
        }
        out.sort();
        out.truncate(max);
        out
    }

    /// La domanda che conta: il contesto della parola migliora la lettura rispetto al glifo isolato?
    ///
    /// Misura appaiata sugli stessi documenti puliti: per ogni parola reale del testo, si rende la
    /// parola coi glifi del font e la si legge; in parallelo si rendono i suoi glifi uno per uno e si
    /// ricompone la lettura carattere per carattere. Verita' attesa: la parola estratta, che su un
    /// documento pulito e' onesta.
    ///
    /// `cargo test --features ocr-specimen contesto -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn contesto_della_parola_vs_glifo_isolato() {
        let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/fp-corpus/clean");
        let pdfs = pdfs_in(&corpus, 4);
        if pdfs.is_empty() {
            eprintln!("skip: corpus assente");
            return;
        }
        let ocr = match crate::ocr::TesseractOcr::new(Some(tessdata()), "eng", OcrHint::SingleLine) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skip: Tesseract non inizializzabile ({e:#})");
                return;
            }
        };

        let (mut parole, mut parole_ok, mut glifi, mut glifi_ok) = (0usize, 0usize, 0usize, 0usize);
        for pdf in &pdfs {
            let Ok(doc) = lopdf::Document::load(pdf) else { continue };
            let Ok(fonts) = crate::pdf_glyph::pdf_font_claims(pdf) else { continue };
            let Some((bytes, claims)) = fonts.first() else { continue };
            let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue };
            let mut c2g: HashMap<char, u16> = HashMap::new();
            for &(ch, gid) in claims.iter() {
                c2g.entry(ch).or_insert(gid);
            }
            let pagine: Vec<u32> = doc.get_pages().keys().copied().take(2).collect();
            let Ok(testo) = doc.extract_text(&pagine) else { continue };

            for w in testo.split_whitespace() {
                let w: String = w.chars().filter(|c| c.is_alphabetic()).collect();
                if w.chars().count() < 4 || w.chars().count() > 12 || parole >= 60 {
                    continue;
                }
                let Some(gids) = w.chars().map(|c| c2g.get(&c).copied()).collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                // (a) parola intera
                let Some(img) = render_word_px(&face, &gids, PX) else { continue };
                parole += 1;
                let letto = ocr.recognize(&img, OcrHint::SingleLine).unwrap_or_default();
                let pulito: String =
                    letto.chars().filter(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
                if pulito == w.to_lowercase() {
                    parole_ok += 1;
                }
                // (b) stessi glifi, letti uno per uno
                for (k, g) in gids.iter().enumerate() {
                    let Some(gi) = render_specimen_px(&face, *g, PX) else { continue };
                    glifi += 1;
                    let sc = ocr.recognize_scored(&gi, OcrHint::SingleLine).unwrap_or_default();
                    let atteso = w.chars().nth(k).unwrap_or(' ');
                    if let Some((l, _)) = sc
                        .iter()
                        .filter_map(|(t, c)| t.chars().find(|c| c.is_alphabetic()).map(|l| (l, *c)))
                        .next()
                    {
                        if crate::glyphmatch::legitimately_identical_ci(l, atteso) {
                            glifi_ok += 1;
                        }
                    }
                }
            }
        }
        let pa = if parole > 0 { parole_ok as f32 / parole as f32 * 100.0 } else { 0.0 };
        let ga = if glifi > 0 { glifi_ok as f32 / glifi as f32 * 100.0 } else { 0.0 };
        println!("
parola intera : {parole_ok}/{parole} = {pa:.1}% (parola letta esattamente)");
        println!("glifo isolato : {glifi_ok}/{glifi} = {ga:.1}% (carattere giusto)");
    }

    /// La prova funzionale: sul documento d'attacco il render della parola deve riconoscere che la
    /// sostituzione e' reale, cioe' stare dalla parte della ricostruzione e non del testo estratto.
    ///
    /// `cargo test --features ocr-specimen direzione -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn direzione_riconosciuta_sul_documento_dattacco() {
        use crate::textdiff::PhraseSide;
        let fx = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/attack-fixtures/replaced.pdf");
        if !fx.exists() {
            eprintln!("skip: fixture assente");
            return;
        }
        let ocr = match crate::ocr::TesseractOcr::new(Some(tessdata()), "eng", OcrHint::SingleLine) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skip: Tesseract ({e:#})");
                return;
            }
        };
        let subs = crate::pdf_glyph::pdf_outline_scan(&fx).expect("scan").substitutions;
        eprintln!("sostituzioni recuperate: {subs:?}");
        let doc = lopdf::Document::load(&fx).expect("load");
        let fonts = crate::pdf_glyph::pdf_font_claims(&fx).expect("claims");
        let pagine: Vec<u32> = doc.get_pages().keys().copied().collect();
        let testo = doc.extract_text(&pagine).unwrap_or_default();

        let inversa: Vec<(char, char)> = subs.iter().map(|&(a, b)| (b, a)).collect();
        eprintln!("mappa inversa:            {inversa:?}");
        for (etichetta, mappa) in [("DIRETTA", &subs), ("INVERSA", &inversa)] {
        let (mut pro_ricostruzione, mut pro_estratto, mut incerte) = (0, 0, 0);
        for (bytes, claims) in fonts.iter() {
            let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue };
            let mut c2g: HashMap<char, u16> = HashMap::new();
            for &(ch, gid) in claims.iter() {
                c2g.entry(ch).or_insert(gid);
            }
            for w in testo.split_whitespace() {
                let w: String = w.chars().filter(|c| c.is_alphabetic()).collect();
                if w.chars().count() < 4 || w.chars().count() > 14 {
                    continue;
                }
                match word_specimen_side(&face, &c2g, &w, mappa, &ocr) {
                    Some(PhraseSide::Presumed) => pro_ricostruzione += 1,
                    Some(PhraseSide::Extracted) => pro_estratto += 1,
                    Some(PhraseSide::Inconclusive) => incerte += 1,
                    None => {}
                }
                if pro_ricostruzione + pro_estratto + incerte >= 25 {
                    break;
                }
            }
        }
        println!(
            "[{etichetta}] ricostruzione={pro_ricostruzione} estratto={pro_estratto} incerte={incerte}"
        );
        }
    }
}
