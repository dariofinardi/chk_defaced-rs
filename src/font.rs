//! Read-only parsing of SFNT fonts (`.ttf`/`.otf`) and collections (`.ttc`/`.otc`) via `ttf-parser`.
//! Extracts identity (name table), counts, and most importantly the **Unicode cmap** (codepoint →
//! glyph id), which is the bridge between "extracted character" and "drawn glyph": the heart of the
//! coherence check.

use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::prelude::*;
use skrifa::raw::{FileRef, FontRef, TableProvider};
use skrifa::string::StringId;
use skrifa::{GlyphId, MetadataProvider};
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// A normalized font face.
#[derive(Debug, Clone)]
pub struct ParsedFont {
    pub family: Option<String>,
    pub subfamily: Option<String>,
    pub full_name: Option<String>,
    pub postscript_name: Option<String>,
    pub version: Option<String>,
    pub copyright: Option<String>,
    pub manufacturer: Option<String>,
    pub num_glyphs: u16,
    pub units_per_em: u16,
    /// Best Unicode cmap: codepoint → glyph id.
    pub cmap: BTreeMap<u32, u16>,
    /// Per-codepoint outline hash for **Latin** letters: codepoint → FNV-1a of the glyph's contour
    /// command stream. The ground truth for the canonical (outline-vs-registry) tamper check.
    pub outlines: BTreeMap<u32, u64>,
    /// SHA-256 of the canonicalized cmap (sorted (cp,gid) pairs, little-endian).
    pub cmap_sha256: String,
    /// SHA-256 of the whole font file (identical for every face of a collection).
    pub file_sha256: String,
    /// Index of the face within a collection (.ttc); 0 for a single font.
    pub collection_index: u32,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Recognized font extensions (lowercase, without the dot).
pub fn is_font_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc")
}

/// Parse every face of a font file. Faces that fail to parse are skipped (no panic).
pub fn parse_file(path: &Path) -> Result<Vec<ParsedFont>> {
    let data = std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?;
    Ok(parse_data(&data))
}

/// Parse every face from the bytes of a font file (handles collections).
pub fn parse_data(data: &[u8]) -> Vec<ParsedFont> {
    let file_sha = sha256_hex(data);
    let count = match FileRef::new(data) {
        Ok(FileRef::Collection(c)) => c.len(),
        Ok(FileRef::Font(_)) => 1,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for i in 0..count {
        if let Ok(face) = FontRef::from_index(data, i) {
            out.push(from_face(&face, &file_sha, i));
        }
    }
    out
}

fn name(face: &FontRef, id: u16) -> Option<String> {
    face.localized_strings(StringId::new(id))
        .map(|s| s.chars().collect::<String>().trim().to_string())
        .find(|s| !s.is_empty())
}

/// Extract the richest Unicode cmap (prefers format 12 over format 4 by picking the one with the
/// most entries).
fn extract_cmap(face: &FontRef) -> BTreeMap<u32, u16> {
    // `skrifa` sceglie da se' la mappa Unicode migliore (preferisce la 12 alla 4) ed espone il
    // risultato gia' fuso: dove `ttf-parser` obbligava a scorrere le sottotabelle e a tenere la piu'
    // ricca, qui basta enumerare le mappature.
    let charmap = face.charmap();
    // `skrifa` **preferisce** la sottotabella simbolica (3,0) quando c'e', mentre il filtro
    // `is_unicode()` del parser precedente la escludeva. Una cmap simbolica mappa per progetto in
    // F000-F0FF, cioe' nella PUA: presa per Unicode fa scattare `FONT.PUA_CMAP` su font legittimi.
    // Misurato: senza questa guardia, un documento pulito su 32 passava a 7 finding High.
    if charmap.is_symbol() {
        return BTreeMap::new();
    }
    charmap.mappings().map(|(cp, gid)| (cp, gid.to_u32() as u16)).collect()
}

fn cmap_hash(cmap: &BTreeMap<u32, u16>) -> String {
    let mut h = Sha256::new();
    for (cp, g) in cmap {
        h.update(cp.to_le_bytes());
        h.update(g.to_le_bytes());
    }
    hex(&h.finalize())
}

/// FNV-1a hash of a glyph's outline command stream (exact font-unit coordinates). Two glyphs with the
/// same shape hash identically — a subset copies the parent's outline verbatim, so a subset's honest
/// glyph hashes the same as the canonical one; a tampered glyph hashes like the letter it really draws.
struct OutlineSig {
    h: u64,
    empty: bool,
    start: Option<(f32, f32)>,
    cur: Option<(f32, f32)>,
}
impl OutlineSig {
    fn new() -> Self {
        Self { h: 0xcbf29ce484222325, empty: true, start: None, cur: None }
    }
    fn mix(&mut self, v: i64) {
        self.empty = false;
        self.h ^= v as u64;
        self.h = self.h.wrapping_mul(0x100000001b3);
    }
}
impl OutlinePen for OutlineSig {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = Some((x, y));
        self.cur = Some((x, y));
        self.mix(1);
        self.mix(x as i64);
        self.mix(y as i64);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cur = Some((x, y));
        self.mix(2);
        self.mix(x as i64);
        self.mix(y as i64);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.cur = Some((x, y));
        self.mix(3);
        for v in [x1, y1, x, y] {
            self.mix(v as i64);
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.cur = Some((x, y));
        self.mix(4);
        for v in [x1, y1, x2, y2, x, y] {
            self.mix(v as i64);
        }
    }
    fn close(&mut self) {
        // Chiusura esplicita: `skrifa` chiude implicitamente il contorno, altri parser emettono
        // prima una linea di ritorno al punto iniziale. Normalizzarla qui rende la firma
        // indipendente dalla convenzione di chi disegna — misurato: senza, il 79% degli hash
        // cambia solo per questo.
        if let (Some(s), Some(c)) = (self.start, self.cur) {
            if s != c {
                self.line_to(s.0, s.1);
            }
        }
        self.mix(5);
    }
}

/// FNV-1a hash of the glyph `gid`'s outline, or `None` for an empty/space glyph. Shared by the
/// registry builder and the canonical check so the hashes are directly comparable.
pub(crate) fn glyph_outline_hash(face: &FontRef, gid: u16) -> Option<u64> {
    let mut b = OutlineSig::new();
    let glyph = face.outline_glyphs().get(GlyphId::from(gid))?;
    glyph.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut b).ok()?;
    (!b.empty).then_some(b.h)
}

/// Outline hashes for the Latin letters in the cmap (the v1.0 scope), keyed by codepoint.
fn extract_outlines(face: &FontRef, cmap: &BTreeMap<u32, u16>) -> BTreeMap<u32, u64> {
    use unicode_script::{Script, UnicodeScript};
    cmap.iter()
        .filter(|(cp, _)| {
            char::from_u32(**cp).map(|c| c.is_alphabetic() && c.script() == Script::Latin).unwrap_or(false)
        })
        .filter_map(|(cp, gid)| glyph_outline_hash(face, *gid).map(|h| (*cp, h)))
        .collect()
}

fn from_face(face: &FontRef, file_sha: &str, index: u32) -> ParsedFont {
    // name_id: 0 copyright, 1 family, 2 subfamily, 4 full, 5 version, 6 postscript, 8 manufacturer
    let cmap = extract_cmap(face);
    let cmap_sha256 = cmap_hash(&cmap);
    let outlines = extract_outlines(face, &cmap);
    ParsedFont {
        family: name(face, 1),
        subfamily: name(face, 2),
        full_name: name(face, 4),
        postscript_name: name(face, 6),
        version: name(face, 5),
        copyright: name(face, 0),
        manufacturer: name(face, 8),
        num_glyphs: face.maxp().map(|m| m.num_glyphs()).unwrap_or(0),
        units_per_em: face.head().map(|h| h.units_per_em()).unwrap_or(0),
        cmap,
        outlines,
        cmap_sha256,
        file_sha256: file_sha.to_string(),
        collection_index: index,
    }
}

impl ParsedFont {
    /// Fraction of mapped codepoints that fall in the Private Use Area (a strong sign of
    /// obfuscation: glyphs are drawn but the extracted codepoint is "private"/unreadable).
    pub fn pua_ratio(&self) -> f32 {
        if self.cmap.is_empty() {
            return 0.0;
        }
        let pua = self.cmap.keys().filter(|cp| is_pua(**cp)).count();
        pua as f32 / self.cmap.len() as f32
    }
}

/// Private Use Area: BMP `U+E000..F8FF` and the two supplementary planes.
pub fn is_pua(cp: u32) -> bool {
    (0xE000..=0xF8FF).contains(&cp)
        || (0xF_0000..=0xF_FFFD).contains(&cp)
        || (0x10_0000..=0x10_FFFD).contains(&cp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pua_classifier() {
        assert!(is_pua(0xE000));
        assert!(is_pua(0xF8FF));
        assert!(!is_pua(0x0041)); // 'A'
        assert!(is_pua(0xF_0001));
    }

    #[test]
    fn hex_roundtrip() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
