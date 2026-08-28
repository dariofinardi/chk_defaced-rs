# Changelog

Formato ispirato a [Keep a Changelog](https://keepachangelog.com/it/1.1.0/); il progetto segue
[SemVer](https://semver.org/lang/it/) con la convenzione dello 0.x, dove **il minor segnala una
rottura**. Le versioni precedenti alla 0.3.0 sono ricostruibili dalla storia git.

## [0.3.2] — non ancora rilasciata

Solo documentazione e metadati: nessun cambio di codice, nessuna rottura.

### Aggiunto

- `BUILD.md`: matrice delle feature con il costo nativo di ciascuna, provenienza di Tesseract /
  `traineddata` / pdfium / webview, tutte le variabili d'ambiente lette dal crate, stato per
  piattaforma, inventario delle dipendenze con le licenze, e quali advisory di `cargo audit` sono
  davvero nel grafo di default.
- Nella documentazione del crate, l'elenco dei **moduli dietro feature che docs.rs non mostra**
  (`specimen`, `atlas`, `render`, `glyph`): esistono, ma il builder costruisce solo le feature di
  default, e chi legge lì non vedeva metà del crate.
- Nel README, un blocco di orientamento con link **assoluti** a GitHub: su crates.io i riferimenti
  relativi non portano da nessuna parte.

### Modificato

- `description` del pacchetto: citava solo la coerenza dei font, mentre il crate copre **due** assi
  speculari — il defacement tipografico e il testo invisibile o camuffato. È il testo che si legge
  nella ricerca di crates.io, cioè il primo filtro con cui qualcuno decide se il crate gli serve.

### Noto e non risolto

- La ricerca automatica dei `traineddata` ha `aarch64` scritto nel percorso, quindi su Windows x64
  non trova la directory installata dal binding: serve `TESSDATA_PREFIX` esplicito. Documentato in
  `BUILD.md`, non ancora corretto.

## [0.3.1] — 2026-08-28

Solo correzioni: nessun cambio di comportamento del rilevamento, nessuna rottura di API.

### Corretto

- Il messaggio di `PDF.FONT_SUBSET_WINDOW_ARTIFACT` conteneva **run di spazi** al suo interno,
  finendo così nel testo che l'utente legge.
- Otto **link rotti nella documentazione**, visibili su docs.rs: riferimenti a moduli dietro
  feature (`crate::specimen`, `crate::atlas`, `crate::render`, `TesseractOcr`) che nel build di
  default di docs.rs non esistono e non si risolvono mai, un link a un elemento privato
  (`glyph_outline_hash`) e l'ambiguità fra la funzione `scan` e il modulo omonimo.
- Nove `needless_borrow` in `scan/pdf.rs`, nella zona toccata dalla migrazione a lopdf 0.44.

### Sicurezza

- `crossbeam-epoch` 0.9.18 → 0.9.20 nel lockfile: **RUSTSEC-2026-0204** (dereferenziazione di un
  puntatore non valido nell'impl `fmt::Pointer`). Era nel grafo di **default**, dove arrivava da
  `lopdf 0.44 → rayon → crossbeam-deque`: è entrata con il bump di lopdf della 0.3.0.
- `time` 0.3.48 → 0.3.55: la precedente era *yanked* dal registry.

### Aggiunto

- Integrazione continua (`.github/workflows/ci.yml`): build, test, clippy con `-D warnings` e
  `cargo doc` con `RUSTDOCFLAGS=-D warnings`, più un job che verifica il build
  `--no-default-features`. Tutti verdi al momento dell'introduzione.

  Il job che **blocca** usa una toolchain **fissata** (1.98.0): con `-D warnings` su `stable` una
  nuova release di clippy può rompere la CI senza che il codice sia cambiato, e il verdetto su una
  PR deve dipendere da ciò che scriviamo noi. Un secondo job gira su `stable` e **avvisa senza
  bloccare**, così i lint nuovi si vedono quando escono invece che quando si alza il pin. Il numero
  fissato è un «testato con», non la MSRV — che il crate continua a non dichiarare.
- Questo changelog.

## [0.3.0] — 2026-08-28

Minor e non patch: per un crate 0.x il salto di minor segnala una rottura, e qui ce ne sono due.

### Rotture di API

- **`PhraseDiff::presumed` passa da `String` a `Option<String>`**, con il nuovo campo
  `presumed_withheld: bool`. La ricostruzione si consegna **solo** sotto `Verdict::Confirmed`;
  altrimenti viene ritirata e il campo sparisce anche dal JSON.

  *Migrazione:* dove si leggeva `ph.presumed` ora serve `ph.presumed.as_deref()`, e l'assenza va
  trattata come «nessuna correzione affidabile», non come stringa vuota. Il motivo è misurato: la
  mappa di sostituzione nasce da un segnale deterministico che non distingue una sostituzione
  semantica reale da un subset font con `ToUnicode` rotto, e applicata al secondo *degrada* testo
  corretto (`ORCHESTRARE → ORCJESTRARE`), fino a far scattare falsi allarmi su un modello di
  guardrail a valle.

- **`lopdf` passa da 0.36 a 0.44**, e il tipo compare nell'API pubblica. Un chiamante che passa il
  proprio `Document` deve stare sulla stessa versione.

### Aggiunto

- `scan::scan_document(&lopdf::Document, label, registry)`: ingresso per chi ha già caricato il
  PDF e non vuole farlo parsare due volte.
- `glyphmatch::subsetting_window`: riconosce la firma di un **subset font con `ToUnicode` sfasato**
  — poche coppie, ciascuna rarissima, i cui caratteri disegnati occupano codepoint contigui — e la
  declassa a `Info` invece di segnalare una sostituzione semantica.
- `glyphmatch::specimen_verdict` e `specimen::specimen_reads`: lo specimen-OCR conserva ora anche
  gli **accordi** fra glifo e carattere estratto, e può quindi **scagionare** un finding
  deterministico, non solo confermarlo. `specimen::confirm_with_specimen` lo applica.
- `specimen::render_word_px` / `word_specimen_side`: rende una **parola** reale coi glifi del font
  invece del glifo isolato. Misurato: dentro la propria parola Tesseract legge 60/60 = 100%, da
  solo 214/411 = 52,1%. ⚠️ Non ancora collegato al flusso di scansione, e con un limite documentato
  (la mappa carattere→glifo ne tiene uno solo, mentre un defacement è proprio il caso in cui lo
  stesso carattere ne ha due).

### Corretto

- **Le legature tipografiche non sono più sostituzioni semantiche.** Un glifo che disegna `ﬁ` ed è
  estratto come `f` è tipografia, non un attacco. Il controllo NFKD esisteva ma chiedeva
  l'*uguaglianza* delle decomposizioni (`"fi"` non è `"f"`); ora vale anche il contenimento.
  Su un documento reale cinque `High` su otto erano legature — e rompendo la contiguità delle
  lettere disegnate impedivano anche a `subsetting_window` di riconoscere le altre tre.
- **La mappa delle sostituzioni era non deterministica.** Cinque chiamate identiche sullo stesso PDF
  davano fino a cinque mappe diverse, a volte con le coppie **invertite**: nel gruppo di collisione
  la lettera "onesta" si sceglieva ordinando per il solo conteggio, che non è un ordine totale,
  sopra una `HashMap` con iterazione randomizzata. Ora a parità decide il codepoint. Resta vero che
  **stabile non è vero**: con prove simmetriche gli outline non possono dire quale lettera sia
  onesta, e serve una conferma esterna.
- **La verifica a render può refutare per-frase**, non solo con una somiglianza media di pagina
  ≥ 0,9 che il rumore OCR non raggiunge quasi mai: se il render legge *quella* frase come l'ha letta
  l'estrattore, ed è così per tutte, è prova **contro** il finding.

### Sicurezza

- `quick-xml` 0.37 → 0.41: **RUSTSEC-2026-0194** (tempo quadratico nel controllo degli attributi
  duplicati) e **RUSTSEC-2026-0195** (allocazione illimitata delle dichiarazioni di namespace in
  `NsReader`), entrambi 7.5. `quick-xml` legge anche i metadati XMP, quindi **vede input PDF**.
- `lopdf` 0.44 ripristina i finding `PDF.TOUNICODE_GARBLED` che la 0.36 **non leggeva affatto**: su
  una fixture d'attacco compaiono quattro segnalazioni in più a parità di font esaminati, perché gli
  stream `ToUnicode` ora vengono letti.

### Misure

Su quattro corpus (10 documenti d'attacco, 48 puliti, 16 di un consumatore a valle) **un solo
documento cambia esito**: quello che aveva originato il lavoro, da `FLAGGED` con 8 `High` a `ok`.
Il conteggio dei `High` sugli attacchi non cala mai. A valle, sullo stesso documento, le pagine
instradate a OCR passano da 14 a 0 e il recall sale dell'86,1% al 92,4%.
