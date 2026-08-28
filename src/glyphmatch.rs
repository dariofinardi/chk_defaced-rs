//! Shared glyph-matching primitives for the defacement detectors ([`crate::pdf_glyph`],
//! [`crate::docx_glyph`]) and the specimen-OCR escalation (`crate::specimen`, feature `ocr-specimen`).
//!
//! Centralizes the **Latin-letter filter** and the **"legitimately identical"** predicate (the
//! homoglyph / cross-script / NFKD / ASCII-base filters that keep the semantic-replacement checks
//! false-positive-free). Previously these lived in three near-identical copies; a filter fix in one
//! could silently miss the others. Outline hashing itself lives in `font::glyph_outline_hash`
//! so the registry builder and the checkers produce directly comparable hashes.

use unicode_normalization::UnicodeNormalization;
use unicode_script::{Script, UnicodeScript};

/// A **Latin-script** alphabetic character, normalized to lowercase. v1.0 scopes the outline
/// cross-reference to Latin (the safety target) — basic plus accented/extended letters (European
/// languages), not just ASCII; other scripts are not analyzed, so they raise neither findings nor
/// false positives.
pub fn letter_latin(cp: u32) -> Option<char> {
    let c = char::from_u32(cp)?;
    if c.is_alphabetic() && c.script() == Script::Latin {
        c.to_lowercase().next()
    } else {
        None
    }
}

/// The TR39 confusable skeleton of a single character (cross-script homoglyphs collapse to one).
pub fn skel(c: char) -> String {
    unicode_security::confusable_detection::skeleton(&c.to_string()).collect()
}

/// Two letters legitimately share an identical glyph (so a collision is *not* tampering) when they are
/// the same letter, TR39 confusables, from **different scripts** (Latin 'b' / Greek 'β' / Cyrillic 'в'
/// — cross-script glyph-sharing, not the within-script A3 swap), **compatibility-equivalent** (NFKD:
/// Arabic presentation forms, ligatures), fold to the same **ASCII base** (ð / đ / ɖ → "d"), or the
/// common lowercase-'l' / 'i' pair. Callers pass already-lowercased letters (see [`letter_latin`]).
pub fn legitimately_identical(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    // Cross-script glyph-sharing is legitimate; the A3 attack swaps letters within one script.
    if a.script() != b.script() {
        return true;
    }
    // Compatibility-equivalent encodings (Arabic presentation forms, ligatures) are the same letter.
    if a.to_string().nfkd().eq(b.to_string().nfkd()) {
        return true;
    }
    // Legatura tipografica contro una delle sue lettere componenti (`f` vs `ﬁ` / `ﬀ` / `ﬃ` …).
    // Il confronto NFKD sopra chiede l'uguaglianza delle decomposizioni: "fi" non e' "f", quindi la
    // legatura passava e ogni PDF impaginato bene produceva finding. Il font disegna la forma
    // composta e l'estrazione restituisce la lettera base: e' tipografia, non sostituzione semantica.
    // Misurato: su un documento reale 5 finding High su 8 erano `f` contro ﬀ/ﬁ/ﬂ/ﬃ/ﬄ, e facevano
    // da schermo agli altri 3. Lo specimen scartava gia' le legature (`is_ligature`), il percorso
    // deterministico no.
    let (da, db): (String, String) =
        (a.to_string().nfkd().collect(), b.to_string().nfkd().collect());
    let (na, nb) = (da.chars().count(), db.chars().count());
    if (na > 1 && nb == 1 && da.contains(&db)) || (nb > 1 && na == 1 && db.contains(&da)) {
        return true;
    }

    // Same ASCII base (ð / đ / ɖ → "d", ø → "o"): confusable variants of one base letter, not a swap.
    if let (Some(x), Some(y)) = (deunicode::deunicode_char(a), deunicode::deunicode_char(b)) {
        if !x.is_empty() && x == y {
            return true;
        }
    }
    let mut p = [a, b];
    p.sort_unstable();
    if matches!((p[0], p[1]), ('i', 'l')) {
        return true;
    }
    skel(a) == skel(b)
}

/// Case-insensitive variant used by the specimen path, where an OCR read and the document's claimed
/// character can differ in case (e.g. OCR reads 'M' for a glyph the document extracts as 'm').
pub fn legitimately_identical_ci(a: char, b: char) -> bool {
    let la = a.to_lowercase().next().unwrap_or(a);
    let lb = b.to_lowercase().next().unwrap_or(b);
    la == lb || legitimately_identical(la, lb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_filter() {
        assert_eq!(letter_latin('A' as u32), Some('a'));
        assert_eq!(letter_latin('é' as u32), Some('é'));
        assert_eq!(letter_latin('Α' as u32), None); // Greek
        assert_eq!(letter_latin('1' as u32), None); // digit
    }

    #[test]
    fn homoglyphs_and_real_swaps() {
        assert!(legitimately_identical('a', 'a'));
        assert!(legitimately_identical('a', '\u{0430}')); // Latin a vs Cyrillic а (cross-script)
        assert!(legitimately_identical('b', '\u{03B2}')); // Latin b vs Greek β
        assert!(legitimately_identical('\u{00F0}', '\u{0111}')); // ð vs đ — same ASCII base
        assert!(legitimately_identical('i', 'l'));
        assert!(!legitimately_identical('m', 'd')); // genuine same-script swap
        assert!(!legitimately_identical('r', 'n'));
    }

    #[test]
    fn case_insensitive_variant() {
        assert!(legitimately_identical_ci('M', 'm'));
        assert!(!legitimately_identical_ci('M', 'D'));
    }
}

/// Numero minimo di coppie perché la finestra di subset sia riconoscibile come tale.
const WINDOW_MIN_PAIRS: usize = 3;
/// Quante volte una coppia può scattare restando compatibile con un artefatto.
///
/// Una sostituzione semantica deve essere **sistematica** per cambiare ciò che un lettore capisce:
/// tre coppie che scattano una volta ciascuna in 126 pagine non spostano il senso di nulla.
const WINDOW_MAX_OCCURRENCES: usize = 2;

/// Riconosce la firma di un **subset font con `ToUnicode` sfasato**: poche coppie, ciascuna rarissima,
/// i cui caratteri *disegnati* occupano codepoint **consecutivi**.
///
/// Complementare a `uniform_shift`, che copre lo shift alfabetico uniforme (il caso LaTeX): lì la
/// distanza estratto→disegnato è costante, qui a essere contigui sono solo i disegnati, perché in un
/// font sottoinsieme i glyph-id vengono assegnati in ordine e una finestra traslata li rimappa in
/// blocco su caratteri d'origine slegati fra loro.
///
/// Il caso misurato a valle (field report, PR #1): `h→j`, `ü→k`, `þ→l`, una occorrenza ciascuna su un
/// documento di 126 pagine. I disegnati `j`, `k`, `l` sono adiacenti; le sorgenti `h`, `ü`, `þ` non lo
/// sono, quindi `uniform_shift` resta muto e il documento veniva marcato `High`.
///
/// Conservativa: chiede **tutte** le condizioni insieme, e una sola coppia frequente la disattiva —
/// un attacco reale è sistematico e non si nasconde qui. Deterministica, nessun OCR, nessun render.
pub fn subsetting_window(pairs: &[(char, char, usize)]) -> Option<String> {
    if pairs.len() < WINDOW_MIN_PAIRS {
        return None;
    }
    if pairs.iter().any(|&(_, _, n)| n > WINDOW_MAX_OCCURRENCES) {
        return None;
    }
    let mut drawn: Vec<u32> = pairs.iter().map(|&(_, truth, _)| truth as u32).collect();
    drawn.sort_unstable();
    drawn.dedup();
    if drawn.len() != pairs.len() {
        return None; // due coppie che disegnano la stessa lettera non formano una finestra
    }
    let span = drawn.last()? - drawn.first()?;
    if span as usize != drawn.len() - 1 {
        return None; // non contigui
    }
    let letters: String = drawn.iter().filter_map(|&c| char::from_u32(c)).collect();
    Some(format!(
        "{} glyph(s) drawing the consecutive letters '{}', each firing at most {}× — the signature of a subset font with a shifted ToUnicode, not a targeted replacement (which has to be systematic to change meaning)",
        pairs.len(),
        letters,
        WINDOW_MAX_OCCURRENCES
    ))
}


/// Cosa dice lo specimen-OCR su un finding deterministico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecimenVerdict {
    /// Il glifo disegna davvero la lettera che il confronto degli outline gli attribuisce.
    Corroborated,
    /// Il glifo disegna la lettera **estratta**: la `cmap`/`ToUnicode` era onesta e l'aggancio
    /// dell'outline era sbagliato.
    Refuted,
    /// Letture assenti o discordanti: lo specimen non decide.
    Inconclusive,
}

/// Confronta le sostituzioni deterministiche con ciò che lo specimen-OCR legge davvero sui glifi.
///
/// `subs` sono le coppie `(estratto, disegnato_secondo_gli_outline)`; `specimen_reads` sono le coppie
/// `(estratto, lettera_letta_dall_OCR_sul_glifo)`, **accordi inclusi** — è il punto: un glifo che
/// l'OCR legge uguale al carattere estratto è prova che il font non mente, e prima veniva scartata
/// perché lo specimen registrava soltanto i disaccordi.
///
/// ⚠️ Limite da tenere presente: lo specimen parla del **glifo**, non della pagina. Dice quale lettera
/// quel disegno rappresenta, non se il testo visibile è stato alterato; per quello resta autorevole la
/// verifica a render ([`crate::textdiff::ocr_refutes_substitutions`]). Serve però a chiudere in
/// anticipo, e senza rasterizzare pagine, i casi in cui l'aggancio dell'outline ha preso un abbaglio.
pub fn specimen_verdict(subs: &[(char, char)], specimen_reads: &[(char, char)]) -> SpecimenVerdict {
    let (mut pro, mut contra) = (0usize, 0usize);
    for &(extracted, claimed_drawn) in subs {
        for &(e, read) in specimen_reads.iter().filter(|(e, _)| *e == extracted) {
            let _ = e;
            if legitimately_identical_ci(read, claimed_drawn) {
                pro += 1;
            } else if legitimately_identical_ci(read, extracted) {
                contra += 1;
            }
        }
    }
    match (pro, contra) {
        (0, 0) => SpecimenVerdict::Inconclusive,
        (0, _) => SpecimenVerdict::Refuted,
        (_, 0) => SpecimenVerdict::Corroborated,
        _ => SpecimenVerdict::Inconclusive, // prove in conflitto: non si decide
    }
}

#[cfg(test)]
mod subsetting_window_tests {
    use super::*;

    /// Il caso misurato: h→j, ü→k, þ→l, una occorrenza ciascuna su 126 pagine.
    fn field_report_case() -> Vec<(char, char, usize)> {
        vec![('h', 'j', 1), ('ü', 'k', 1), ('þ', 'l', 1)]
    }

    #[test]
    fn riconosce_la_finestra_di_subset_del_field_report() {
        let reason = subsetting_window(&field_report_case()).expect("va riconosciuta come artefatto");
        assert!(reason.contains("jkl"), "la spiegazione deve mostrare le lettere contigue: {reason}");
    }

    // ---- ciò che NON deve essere soppresso ------------------------------------------------

    #[test]
    fn una_sostituzione_sistematica_non_e_un_artefatto() {
        // Stesse lettere contigue, ma che scattano molte volte: è il profilo di un attacco reale,
        // che deve restare High. È il test che impedisce a questa regola di diventare un buco.
        let attacco = vec![('h', 'j', 40), ('ü', 'k', 12), ('þ', 'l', 9)];
        assert!(subsetting_window(&attacco).is_none());
    }

    #[test]
    fn basta_una_coppia_frequente_per_disattivare_la_regola() {
        let misto = vec![('h', 'j', 1), ('ü', 'k', 1), ('þ', 'l', 3)];
        assert!(subsetting_window(&misto).is_none(), "una sola coppia frequente deve bastare");
    }

    #[test]
    fn lettere_disegnate_non_contigue_non_sono_una_finestra() {
        // Il caso chirurgico: poche lettere scelte per trasformare una parola in un'altra.
        let chirurgico = vec![('l', 'i', 1), ('o', 'e', 1), ('n', 'a', 1)];
        assert!(subsetting_window(&chirurgico).is_none());
    }

    #[test]
    fn due_coppie_non_bastano() {
        assert!(subsetting_window(&[('h', 'j', 1), ('ü', 'k', 1)]).is_none());
        assert!(subsetting_window(&[]).is_none());
    }

    #[test]
    fn coppie_che_disegnano_la_stessa_lettera_non_formano_una_finestra() {
        let doppione = vec![('h', 'j', 1), ('ü', 'j', 1), ('þ', 'k', 1)];
        assert!(subsetting_window(&doppione).is_none());
    }

    #[test]
    fn la_finestra_regge_a_ordine_sparso_e_a_finestre_piu_lunghe() {
        let sparso = vec![('þ', 'l', 1), ('h', 'j', 2), ('ü', 'k', 1)];
        assert!(subsetting_window(&sparso).is_some(), "l'ordine delle coppie non deve contare");
        let lunga = vec![('a', 'p', 1), ('b', 'q', 1), ('c', 'r', 1), ('d', 's', 1), ('e', 't', 1)];
        assert!(subsetting_window(&lunga).is_some());
    }
}

#[cfg(test)]
mod specimen_verdict_tests {
    use super::*;

    /// Le sostituzioni deterministiche del field report: l'estratto 'h' disegnerebbe 'j'.
    const SUBS: [(char, char); 1] = [('h', 'j')];

    #[test]
    fn lo_specimen_scagiona_quando_il_glifo_disegna_la_lettera_estratta() {
        // L'OCR del glifo legge 'h': la ToUnicode era onesta e l'aggancio dell'outline sbagliato.
        // È la prova d'accordo che prima veniva scartata sul posto.
        assert_eq!(specimen_verdict(&SUBS, &[('h', 'h')]), SpecimenVerdict::Refuted);
    }

    #[test]
    fn lo_specimen_conferma_quando_il_glifo_disegna_davvero_l_altra_lettera() {
        assert_eq!(specimen_verdict(&SUBS, &[('h', 'j')]), SpecimenVerdict::Corroborated);
    }

    #[test]
    fn senza_letture_non_si_decide() {
        assert_eq!(specimen_verdict(&SUBS, &[]), SpecimenVerdict::Inconclusive);
        // Lettura su un carattere che non c'entra: non tocca il finding.
        assert_eq!(specimen_verdict(&SUBS, &[('z', 'z')]), SpecimenVerdict::Inconclusive);
        // Lettura che non è né l'estratto né il disegnato dichiarato.
        assert_eq!(specimen_verdict(&SUBS, &[('h', 'w')]), SpecimenVerdict::Inconclusive);
    }

    #[test]
    fn prove_in_conflitto_non_decidono() {
        // Due glifi per lo stesso carattere, letti in modo opposto: il dubbio non si risolve
        // inventando una risposta.
        let verdict = specimen_verdict(&SUBS, &[('h', 'h'), ('h', 'j')]);
        assert_eq!(verdict, SpecimenVerdict::Inconclusive);
    }

    #[test]
    fn gli_omoglifi_non_creano_falsi_disaccordi() {
        // 'H' latina letta per 'h': stessa lettera, non una smentita.
        assert_eq!(specimen_verdict(&SUBS, &[('h', 'H')]), SpecimenVerdict::Refuted);
    }

    #[test]
    fn piu_sostituzioni_votano_insieme() {
        let subs = [('h', 'j'), ('ü', 'k'), ('þ', 'l')];
        let reads = [('h', 'h'), ('ü', 'ü'), ('þ', 'þ')];
        assert_eq!(specimen_verdict(&subs, &reads), SpecimenVerdict::Refuted);
    }
}

#[cfg(test)]
mod ligature_tests {
    use super::*;

    /// Le legature latine: U+FB00 ﬀ, FB01 ﬁ, FB02 ﬂ, FB03 ﬃ, FB04 ﬄ, FB06 ﬆ.
    #[test]
    fn una_legatura_non_e_una_sostituzione() {
        for lig in ['\u{FB00}', '\u{FB01}', '\u{FB02}', '\u{FB03}', '\u{FB04}'] {
            assert!(legitimately_identical('f', lig), "f vs {lig:?} deve essere legittimo");
            assert!(legitimately_identical(lig, 'f'), "e simmetrico");
        }
        assert!(legitimately_identical('s', '\u{FB06}'), "st: la legatura inizia per s");
    }

    #[test]
    fn il_filtro_non_scusa_gli_scambi_veri() {
        // Le coppie che il rilevatore deve continuare a vedere.
        for (a, b) in [('m', 'd'), ('h', 'j'), ('r', 'l'), ('w', 'l'), ('a', 'e')] {
            assert!(!legitimately_identical(a, b), "{a} vs {b} deve restare una sostituzione");
        }
        // Una legatura contro una lettera che non la compone resta un sospetto.
        assert!(!legitimately_identical('m', '\u{FB01}'), "m non compone 'fi'");
    }
}
