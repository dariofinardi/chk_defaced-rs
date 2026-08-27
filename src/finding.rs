//! Serializable report model (aligned with the project plan, simplified for v1).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    FontIntegrity,
    UnicodeHygiene,
    HiddenContent,
    Structural,
}

/// Render-level OCR verdict on a semantic-replacement (collision) signal — the second, independent
/// opinion on top of the deterministic finding. `Confirmed`: the rendered text really diverges from the
/// extracted text. `Refuted`: they match (the font carries a glyph collision but it is not used on the
/// visible text — a loaded-but-unfired rig). `Unconfirmed`: OCR was not run, or was inconclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Confirmed,
    Refuted,
    Unconfirmed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    pub category: Category,
    pub message: String,
    /// Where (free-form: page/part/selector/font), human-readable.
    pub location: String,
    pub confidence: f32,
}

impl Finding {
    pub fn new(
        rule: &str,
        severity: Severity,
        category: Category,
        location: impl Into<String>,
        message: impl Into<String>,
        confidence: f32,
    ) -> Self {
        Finding {
            rule: rule.to_string(),
            severity,
            category,
            location: location.into(),
            message: message.into(),
            confidence,
        }
    }
}

/// Explicit, machine-readable document-level verdict, computed from the findings — so a consumer does
/// not have to re-derive "is this clean / defaced / hiding text" from the raw list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    /// No High-or-worse finding of any kind.
    pub ok: bool,
    /// Font tampering (glyph/cmap/ToUnicode) at High+ — the rendered text can differ from the extracted.
    pub defaced: bool,
    /// Text present in the extraction but not visible to a human (Medium+ HiddenContent) — the
    /// prompt-injection / hidden-clause vector.
    pub hidden_text: bool,
    /// The worst severity present, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_severity: Option<Severity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub file: String,
    pub format: String,
    pub fonts_examined: usize,
    /// Provenance metadata (author / authoring tool / producer / company / dates) to evaluate the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<crate::metadata::DocumentMetadata>,
    /// Explicit document-level verdict computed from `findings` (see [`Report::finalize`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<Assessment>,
    pub findings: Vec<Finding>,
    /// The sentences affected by a semantic replacement — each with the **extracted** text (what a
    /// RAG/LLM reads), the **presumed** rendered text (substitutions applied), and, when OCR is run on a
    /// PDF, the **ocr** ground-truth. Only differing sentences are listed, for easy side-by-side compare.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phrases: Vec<PhraseDiff>,
    /// The render-level OCR verdict on the semantic-replacement findings (when any). The deterministic
    /// finding is kept regardless; this is the independent confirmation that drives the final severity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

/// One affected sentence: the extracted (read) text vs the presumed-correct (rendered) reconstruction,
/// plus the 1-based **page** it was found on (PDF) and the optional OCR reading that resolves which one
/// is actually on the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhraseDiff {
    /// What an extractor / RAG reads (the lie, for a defacement).
    pub extracted: String,
    /// What the glyphs presumably render, by applying the recovered substitution map.
    ///
    /// **`None` means the reconstruction was withheld**, not that there is none: a substitution map
    /// recovered from a deterministic signal is a *candidate*, and applying it without corroboration
    /// can make the text worse than the extraction (a subset font with a broken `ToUnicode` yields
    /// `ORCHESTRARE -> ORCJESTRARE`). Present only when [`Verdict::Confirmed`] — see
    /// [`Report::withdraw_uncorroborated_presumed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presumed: Option<String>,
    /// `true` when a reconstruction existed but was withheld for lack of corroboration. Distinguishes
    /// "nothing to correct" from "correction not trustworthy", which a consumer must not confuse.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub presumed_withheld: bool,
    /// 1-based page the sentence occurs on (PDF only; `None` for the page-less DOCX flow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// OCR of the rendered text (ground truth) — present only when OCR was run (`--ocr`, PDF).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr: Option<String>,
}

impl Report {
    pub fn new(file: &str, format: &str) -> Self {
        Report {
            file: file.to_string(),
            format: format.to_string(),
            fonts_examined: 0,
            metadata: None,
            assessment: None,
            findings: Vec::new(),
            phrases: Vec::new(),
            verdict: None,
        }
    }
    pub fn push(&mut self, f: Finding) {
        self.findings.push(f);
    }
    pub fn max_severity(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).max()
    }

    /// Compute the explicit [`Assessment`] from the current findings. Idempotent — call it after the
    /// deterministic scan and again after any OCR escalation (which may raise severities). `defaced` =
    /// a High+ `FontIntegrity` finding; `hidden_text` = a Medium+ `HiddenContent` finding.
    /// Withdraw every `presumed` reconstruction that no independent evidence corroborates.
    ///
    /// The substitution map comes from a deterministic outline signal, which cannot tell a real
    /// semantic replacement from a subset font whose `ToUnicode` lies. Applied to the second, it
    /// *degrades* correct text — and a downstream guardrail then flags the damage the correction
    /// itself produced. So the reconstruction is handed out only under [`Verdict::Confirmed`];
    /// otherwise it is withheld and the phrase says so via `presumed_withheld`.
    ///
    /// Called by [`Report::finalize`] and at the end of the render verification, so the invariant
    /// holds on every path: **`presumed.is_some()` implies corroborated**.
    pub fn withdraw_uncorroborated_presumed(&mut self) {
        if self.verdict == Some(Verdict::Confirmed) {
            return;
        }
        for ph in &mut self.phrases {
            if ph.presumed.take().is_some() {
                ph.presumed_withheld = true;
            }
        }
    }

    pub fn finalize(&mut self) {
        self.withdraw_uncorroborated_presumed();
        let max = self.max_severity();
        let defaced = self
            .findings
            .iter()
            .any(|f| matches!(f.category, Category::FontIntegrity) && f.severity >= Severity::High);
        let hidden_text = self
            .findings
            .iter()
            .any(|f| matches!(f.category, Category::HiddenContent) && f.severity >= Severity::Medium);
        self.assessment = Some(Assessment {
            ok: max.is_none_or(|s| s < Severity::High),
            defaced,
            hidden_text,
            max_severity: max,
        });
    }
}

/// Result of an outline cross-reference scan: the findings plus the recovered substitution map —
/// `(extracted_char, corrected_char)` pairs — used to reconstruct the presumed-correct text.
pub struct OutlineScan {
    pub findings: Vec<Finding>,
    pub substitutions: Vec<(char, char)>,
}

/// Collapse runs of whitespace to single spaces, trim, and cap length (keeps reports readable and
/// bounded). The same cleaning is applied to the extracted text before substitution so the corrected
/// text stays character-aligned with it.
pub fn clean_snippet(text: &str, max_chars: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > max_chars {
        let mut s: String = collapsed.chars().take(max_chars).collect();
        s.push('…');
        s
    } else {
        collapsed
    }
}

/// Apply glyph substitutions to extracted text to recover what is presumably rendered. The map is
/// lowercase `extracted → corrected`; case is preserved per character.
pub fn apply_substitutions(text: &str, subs: &[(char, char)]) -> String {
    use std::collections::HashMap;
    let map: HashMap<char, char> = subs.iter().copied().collect();
    text.chars()
        .map(|c| {
            let lower = c.to_lowercase().next().unwrap_or(c);
            match map.get(&lower) {
                Some(&corr) if c.is_uppercase() => corr.to_uppercase().next().unwrap_or(corr),
                Some(&corr) => corr,
                None => c,
            }
        })
        .collect()
}

/// Split text into sentences (on `.`, `!`, `?`, `;`, newline), whitespace-collapsed and trimmed.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?' | ';' | '\n' | '\r') {
            let s = clean_snippet(&cur, 400);
            if !s.is_empty() {
                out.push(s);
            }
            cur.clear();
        }
    }
    let s = clean_snippet(&cur, 400);
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// Per-sentence diffs for the report: split the extracted text into sentences and keep only those a
/// substitution actually changes, each paired with its presumed-rendered form. Capped to keep reports
/// bounded.
pub fn phrase_diffs(extracted_text: &str, subs: &[(char, char)]) -> Vec<PhraseDiff> {
    phrase_diffs_paged(extracted_text, subs, None)
}

/// [`phrase_diffs`] tagging each returned phrase with the given (1-based) page number.
pub fn phrase_diffs_paged(extracted_text: &str, subs: &[(char, char)], page: Option<u32>) -> Vec<PhraseDiff> {
    split_sentences(extracted_text)
        .into_iter()
        .filter_map(|s| {
            let presumed = apply_substitutions(&s, subs);
            if presumed != s {
                Some(PhraseDiff {
                    extracted: s,
                    presumed: Some(presumed),
                    presumed_withheld: false,
                    page,
                    ocr: None,
                })
            } else {
                None
            }
        })
        .take(100)
        .collect()
}

#[cfg(test)]
mod presumed_gating_tests {
    use super::*;

    /// Il caso reale del field report: un subset font con `ToUnicode` rotto produce la mappa
    /// `h→j`, che applicata *peggiora* testo corretto.
    const DAMAGING_SUBS: [(char, char); 1] = [('h', 'j')];
    const SENTENCE: &str = "Auspichiamo di orchestrare i collegi che decidono.";

    fn report_with_phrases(verdict: Option<Verdict>) -> Report {
        let mut r = Report::new("deck.pdf", "pdf");
        r.phrases = phrase_diffs(SENTENCE, &DAMAGING_SUBS);
        r.verdict = verdict;
        r
    }

    #[test]
    fn la_ricostruzione_candidata_e_davvero_peggiorativa() {
        // Non e' un'ipotesi: la mappa recuperata danneggia il testo. Se un giorno questo test
        // fallisse, l'esempio non varrebbe piu' come regressione.
        let phrases = phrase_diffs(SENTENCE, &DAMAGING_SUBS);
        let presumed = phrases[0].presumed.as_deref().unwrap();
        assert!(presumed.contains("Auspicjiamo"), "{presumed}");
        assert!(presumed.contains("orcjestrare"), "{presumed}");
        assert!(presumed.contains("cje"), "{presumed}");
    }

    #[test]
    fn confermato_la_ricostruzione_resta() {
        let mut r = report_with_phrases(Some(Verdict::Confirmed));
        r.finalize();
        assert!(r.phrases[0].presumed.is_some(), "sotto Confirmed la ricostruzione va consegnata");
        assert!(!r.phrases[0].presumed_withheld);
    }

    #[test]
    fn non_confermato_la_ricostruzione_e_ritirata() {
        // Il caso misurato a valle: il render lascia Unconfirmed, e prima di questa modifica il
        // consumatore riceveva comunque la "correzione" che ha fabbricato quattro falsi allarmi.
        for verdict in [None, Some(Verdict::Unconfirmed), Some(Verdict::Refuted)] {
            let mut r = report_with_phrases(verdict);
            r.finalize();
            let ph = &r.phrases[0];
            assert!(ph.presumed.is_none(), "verdetto {verdict:?}: la ricostruzione non va consegnata");
            assert!(ph.presumed_withheld, "verdetto {verdict:?}: il ritiro va dichiarato");
            assert_eq!(ph.extracted, SENTENCE, "il testo estratto resta intatto");
        }
    }

    #[test]
    fn il_ritiro_e_idempotente_e_non_resuscita() {
        let mut r = report_with_phrases(Some(Verdict::Unconfirmed));
        r.finalize();
        r.finalize();
        assert!(r.phrases[0].presumed.is_none());
        assert!(r.phrases[0].presumed_withheld);
    }

    #[test]
    fn nel_json_il_campo_sparisce_invece_di_mentire() {
        // Un consumatore che legge il JSON non deve trovare una `presumed` non corroborata: il campo
        // assente e' l'unico segnale che non si presta a essere usato per sbaglio.
        let mut r = report_with_phrases(Some(Verdict::Unconfirmed));
        r.finalize();
        let js = serde_json::to_string(&r).unwrap();
        assert!(!js.contains("\"presumed\":"), "{js}");
        assert!(js.contains("\"presumed_withheld\":true"), "{js}");

        let mut ok = report_with_phrases(Some(Verdict::Confirmed));
        ok.finalize();
        let js_ok = serde_json::to_string(&ok).unwrap();
        assert!(js_ok.contains("\"presumed\":"), "{js_ok}");
        assert!(!js_ok.contains("presumed_withheld"), "{js_ok}");
    }
}
