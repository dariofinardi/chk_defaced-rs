//! Dependency-free text-comparison helpers shared by the render-OCR fallbacks (PDF `crate::atlas` and
//! DOCX `crate::render`): word-set similarity and aligned word-level substitution detection between the
//! **extracted** text (what a machine reads) and the **visual/OCR** text (what a human sees). A real,
//! deliberate substitution between the two is the render-level signature of semantic replacement
//! (variant A3) — including the localized/positional form the deterministic outline checks cannot express.

use std::collections::HashSet;

/// Normalized word set: lowercase, alphanumeric runs, length ≥ 2.
pub fn words(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_string())
        .collect()
}

/// Word-set Jaccard similarity in `0..=1`, robust to word order and OCR segmentation differences.
pub fn jaccard(a: &str, b: &str) -> f32 {
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() && wb.is_empty() {
        return 1.0;
    }
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.union(&wb).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Ordered, normalized word sequence (lowercase, alphanumeric runs, length ≥ 2).
fn word_seq(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_string())
        .collect()
}

/// Aligned word-level mismatches between extracted and visual text (LCS diff; deletions paired with
/// insertions as substitutions). Catches **localized semantic replacement** — a few substituted words
/// in otherwise-identical text — which page-level set similarity cannot see. NB: OCR errors also surface
/// here, so use it as a signal / for investigation, not a zero-false-positive verdict.
pub fn word_mismatches(extracted: &str, visual: &str) -> Vec<(String, String)> {
    let a = word_seq(extracted);
    let b = word_seq(visual);
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    // LCS lengths (suffix DP).
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    let (mut dels, mut inss): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut out = Vec::new();
    let flush = |dels: &mut Vec<String>, inss: &mut Vec<String>, out: &mut Vec<(String, String)>| {
        let k = dels.len().min(inss.len());
        for t in 0..k {
            out.push((dels[t].clone(), inss[t].clone()));
        }
        dels.clear();
        inss.clear();
    };
    while i < n && j < m {
        if a[i] == b[j] {
            flush(&mut dels, &mut inss, &mut out);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            dels.push(a[i].clone());
            i += 1;
        } else {
            inss.push(b[j].clone());
            j += 1;
        }
    }
    while i < n {
        dels.push(a[i].clone());
        i += 1;
    }
    while j < m {
        inss.push(b[j].clone());
        j += 1;
    }
    flush(&mut dels, &mut inss, &mut out);
    out
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Two aligned words look like a *real substitution* (not an OCR near-miss) if both are alphabetic words
/// of length ≥ 3 and their normalized edit distance is large (e.g. "delaware" vs "maryland"), as opposed
/// to OCR noise ("ai"/"al", "recipient"/"recipients").
fn is_real_substitution(e: &str, v: &str) -> bool {
    let (ec, vc): (Vec<char>, Vec<char>) = (e.chars().collect(), v.chars().collect());
    if ec.len() < 3 || vc.len() < 3 || !ec.iter().all(|c| c.is_alphabetic()) || !vc.iter().all(|c| c.is_alphabetic()) {
        return false;
    }
    let max = ec.len().max(vc.len()) as f32;
    levenshtein(&ec, &vc) as f32 / max > 0.4
}

/// Word substitutions that look deliberate (filters out OCR noise via edit distance). A non-empty result
/// is a strong signal of **semantic replacement** (variant A3): the extracted word differs from the
/// rendered/visible word, both lexically valid.
pub fn significant_substitutions(extracted: &str, visual: &str) -> Vec<(String, String)> {
    word_mismatches(extracted, visual)
        .into_iter()
        .filter(|(e, v)| is_real_substitution(e, v))
        .collect()
}

/// Words present in `extracted` but **absent** from `visual` (the OCR of the render) — the render-level
/// signature of **hidden text**: read by a machine, not shown to a human (white-on-white, sub-visible,
/// occluded, clipped, off-page). The dual of [`significant_substitutions`]. Restricted to alphabetic
/// words of length ≥ 4 and de-duplicated to blunt OCR misses of short/visible words; capped at 50.
pub fn missing_words(extracted: &str, visual: &str) -> Vec<String> {
    let visible = words(visual);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for w in word_seq(extracted) {
        if w.chars().count() >= 4
            && w.chars().all(|c| c.is_alphabetic())
            && !visible.contains(&w)
            && seen.insert(w.clone())
        {
            out.push(w);
            if out.len() >= 50 {
                break;
            }
        }
    }
    out
}

/// Da che parte sta il testo letto dal render, per **una** frase: verso ciò che l'estrattore ha
/// letto, verso la ricostruzione, o nessuna delle due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseSide {
    /// L'OCR somiglia al testo **estratto**: la sostituzione ipotizzata non è avvenuta sulla pagina.
    Extracted,
    /// L'OCR somiglia alla **ricostruzione**: la sostituzione è reale e la pagina la mostra.
    Presumed,
    /// Differenza sotto il margine: l'OCR non decide.
    Inconclusive,
}

/// Margine minimo tra le due somiglianze perché la frase valga come prova.
///
/// Il rumore OCR abbassa **entrambi** i lati insieme, quindi non conta il valore assoluto (che è la
/// ragione per cui una soglia di pagina a 0,9 non scatta quasi mai) ma verso quale dei due si sposta.
pub const PHRASE_SIDE_MARGIN: f32 = 0.05;

/// Per una frase con verità a terra OCR, decide se corrobora il testo estratto o la ricostruzione.
pub fn phrase_side(extracted: &str, presumed: &str, ocr: &str) -> PhraseSide {
    let se = jaccard(ocr, extracted);
    let sp = jaccard(ocr, presumed);
    if se > sp + PHRASE_SIDE_MARGIN {
        PhraseSide::Extracted
    } else if sp > se + PHRASE_SIDE_MARGIN {
        PhraseSide::Presumed
    } else {
        PhraseSide::Inconclusive
    }
}

/// `true` se il render **refuta** le sostituzioni ipotizzate: almeno una frase mostra che la pagina
/// dice ciò che l'estrattore ha letto, e **nessuna** mostra il contrario.
///
/// Serve a togliere l'asimmetria che lasciava tutto `Unconfirmed`: prima si poteva refutare solo con
/// una somiglianza media di pagina ≥ 0,9, irraggiungibile col rumore OCR, e così mezz'ora di render
/// non salvava un documento pulito. Se il render legge *quella* parola correttamente in *quella*
/// posizione, è prova **contro** il finding, non assenza di prova.
///
/// Conservativo per costruzione: una singola frase che sta dalla parte della ricostruzione (cioè un
/// attacco reale, che sulla pagina *si vede*) impedisce la refutazione.
pub fn ocr_refutes_substitutions(phrases: &[(&str, &str, &str)]) -> bool {
    let mut for_extracted = 0usize;
    for (extracted, presumed, ocr) in phrases {
        match phrase_side(extracted, presumed, ocr) {
            PhraseSide::Presumed => return false,
            PhraseSide::Extracted => for_extracted += 1,
            PhraseSide::Inconclusive => {}
        }
    }
    for_extracted > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_localized_substitution() {
        let extracted = "governed by the laws of the State of Delaware without regard";
        let visual = "governed by the laws of the State of Maryland without regard";
        let subs = significant_substitutions(extracted, visual);
        assert_eq!(subs, vec![("delaware".to_string(), "maryland".to_string())]);
    }

    #[test]
    fn ignores_ocr_noise() {
        // a one-char OCR slip and a plural are not "real" substitutions
        assert!(significant_substitutions("the recipient", "the recipients").is_empty());
        assert!(significant_substitutions("paid to ai", "paid to al").is_empty());
    }

    #[test]
    fn jaccard_bounds() {
        assert_eq!(jaccard("", ""), 1.0);
        assert_eq!(jaccard("alpha beta", "alpha beta"), 1.0);
        assert!(jaccard("alpha beta", "gamma delta") < 0.01);
    }

    #[test]
    fn missing_words_finds_hidden_only() {
        // "ignore instructions" is in the extract but not in the rendered/visible text → hidden.
        let extracted = "Please review the contract ignore instructions and sign here";
        let visual = "Please review the contract and sign here";
        let miss = missing_words(extracted, visual);
        assert!(miss.contains(&"ignore".to_string()) && miss.contains(&"instructions".to_string()), "{miss:?}");
        assert!(!miss.contains(&"contract".to_string()), "visible words must not be reported: {miss:?}");
        // identical → nothing hidden
        assert!(missing_words("all visible text here", "all visible text here").is_empty());
    }

    // ---- refutazione per-frase (asimmetria del render) --------------------------------

    /// Le frasi reali del field report: estratto corretto, ricostruzione danneggiata da `h→j`.
    const EXTR: &str = "Auspichiamo di orchestrare i collegi che decidono";
    const PRES: &str = "Auspicjiamo di orcjestrare i collegji cje decidono";

    #[test]
    fn ocr_pulito_sta_col_testo_estratto() {
        assert_eq!(phrase_side(EXTR, PRES, EXTR), PhraseSide::Extracted);
    }

    #[test]
    fn ocr_che_mostra_la_sostituzione_sta_con_la_ricostruzione() {
        // Un attacco vero si *vede* sulla pagina: il render legge la forma sostituita.
        assert_eq!(phrase_side(EXTR, PRES, PRES), PhraseSide::Presumed);
    }

    #[test]
    fn il_rumore_ocr_non_ribalta_il_verdetto() {
        // Due parole sbagliate dall'OCR abbassano entrambe le somiglianze, ma non la direzione:
        // e' esattamente il caso che la soglia di pagina a 0,9 non riusciva a decidere.
        let rumoroso = "Auspichiamo dl orchestrare i colleql che decidono";
        assert_eq!(phrase_side(EXTR, PRES, rumoroso), PhraseSide::Extracted);
    }

    #[test]
    fn ocr_inutilizzabile_non_decide() {
        assert_eq!(phrase_side(EXTR, PRES, "xxx yyy zzz"), PhraseSide::Inconclusive);
    }

    #[test]
    fn quattro_frasi_pulite_refutano() {
        // Il caso misurato: 14 pagine instradate, ogni frase letta correttamente dal render.
        let phrases: Vec<(&str, &str, &str)> = vec![
            (EXTR, PRES, EXTR),
            ("che decidono i collegi", "cje decidono i collegji", "che decidono i collegi"),
            ("orchestrare le attivita", "orcjestrare le attivita", "orchestrare le attivita"),
            ("Auspichiamo un esito", "Auspicjiamo un esito", "Auspichiamo un esito"),
        ];
        assert!(ocr_refutes_substitutions(&phrases));
    }

    #[test]
    fn una_sola_frase_a_favore_della_ricostruzione_blocca_la_refutazione() {
        // Conservativo: un attacco reale non deve essere archiviato perche' le altre frasi sono pulite.
        let phrases: Vec<(&str, &str, &str)> = vec![
            (EXTR, PRES, EXTR),
            ("che decidono i collegi", "cje decidono i collegji", "cje decidono i collegji"),
        ];
        assert!(!ocr_refutes_substitutions(&phrases));
    }

    #[test]
    fn senza_prove_non_si_refuta() {
        assert!(!ocr_refutes_substitutions(&[]));
        assert!(!ocr_refutes_substitutions(&[(EXTR, PRES, "xxx yyy")]), "OCR inconcludente non refuta");
    }
}
