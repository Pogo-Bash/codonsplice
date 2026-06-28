//! Local variant annotation (the `ANNOTATE WITH ...` clause).
//!
//! Joins each [`Variant`] against **local** annotation databases by genomic
//! position — no live APIs (the offline/WASM thesis). Two database kinds are
//! supported:
//!
//! * **gene model (GFF3)** — coordinate-overlap join with INCLUSIVE boundaries
//!   (`start <= pos <= end`), so a variant on an exon edge is *in* the exon.
//!   Yields `gene`, `transcript`, `exon` (rank), `exon_id`, and `region`
//!   (`exon`/`intron`).
//! * **ClinVar (VCF, plain or BGZF)** — exact `(chrom, pos, ref, alt)` match.
//!   Yields `clinvar_significance` (CLNSIG, falling back to ONC so
//!   somatic/oncogenic records survive), `clinvar_oncogenic` (ONC),
//!   `consequence` (MC), `clinvar_id`, and `rsid`.
//!
//! The annotator is built once (parsing the database buffers) and applied per
//! record by [`crate::materialize`]. `aa_change`/HGVS-protein is intentionally
//! out of scope (see `docs/design/ANNOTATE.md`); `consequence` is provided
//! instead.

use std::collections::HashMap;
use std::io::Read;

use cnvlens_core::error::CoreError;
use cnvlens_core::model::Variant;

/// The ordered annotation column names every annotated record carries. A column
/// with no join hit is filled with `"."` (not absent), so predicates like
/// `WHERE clinvar_significance != "."` behave predictably.
pub const ANNOTATION_FIELDS: &[&str] = &[
    "gene",
    "transcript",
    "exon",
    "exon_id",
    "region",
    "consequence",
    "clinvar_significance",
    "clinvar_oncogenic",
    "clinvar_id",
    "rsid",
];

const NA: &str = ".";

/// A loaded set of local annotation databases, ready to join variants against.
#[derive(Debug, Default)]
pub struct Annotator {
    genes: Vec<Gene>,
    /// transcript id → metadata, for ranking overlapping exons.
    transcripts: HashMap<String, Transcript>,
    exons: Vec<Exon>,
    /// (chrom, pos, ref, alt) → ClinVar fields.
    clinvar: HashMap<(String, i64, String, String), ClinRecord>,
}

#[derive(Debug)]
struct Gene {
    chrom: String,
    start: i64,
    end: i64,
    name: String,
}

#[derive(Debug)]
struct Transcript {
    span: i64,
    has_ccds: bool,
}

#[derive(Debug)]
struct Exon {
    chrom: String,
    start: i64,
    end: i64,
    transcript: String,
    rank: String,
    exon_id: String,
}

#[derive(Debug)]
struct ClinRecord {
    significance: String,
    oncogenic: String,
    consequence: String,
    id: String,
    rsid: String,
}

impl Annotator {
    /// Build an annotator from the raw bytes of a GFF3 gene model and/or a
    /// ClinVar VCF (plain text or BGZF — transparently inflated). Either source
    /// may be `None`; the corresponding columns then resolve to `"."`.
    pub fn from_sources(genes: Option<&[u8]>, clinvar: Option<&[u8]>) -> Result<Self, CoreError> {
        let mut a = Annotator::default();
        if let Some(bytes) = genes {
            a.load_gff(&decode_text(bytes)?);
        }
        if let Some(bytes) = clinvar {
            a.load_clinvar(&decode_text(bytes)?);
        }
        Ok(a)
    }

    fn load_gff(&mut self, text: &str) {
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let c: Vec<&str> = line.split('\t').collect();
            if c.len() < 9 {
                continue;
            }
            let (chrom, kind, attrs) = (c[0], c[2], c[8]);
            let (start, end) = match (c[3].parse::<i64>(), c[4].parse::<i64>()) {
                (Ok(s), Ok(e)) => (s, e),
                _ => continue,
            };
            match kind {
                "gene" => {
                    if let Some(name) = gff_attr(attrs, "Name") {
                        self.genes.push(Gene {
                            chrom: chrom.to_string(),
                            start,
                            end,
                            name,
                        });
                    }
                }
                // mRNA/transcript features record the transcript's span and
                // whether it is a CCDS (canonical coding) transcript, used to
                // rank overlapping exons.
                "mRNA" | "transcript" => {
                    if let Some(id) = gff_attr(attrs, "transcript_id")
                        .or_else(|| gff_attr(attrs, "ID").map(strip_prefix_colon))
                    {
                        self.transcripts.insert(
                            id,
                            Transcript {
                                span: end - start,
                                has_ccds: gff_attr(attrs, "ccdsid").is_some(),
                            },
                        );
                    }
                }
                "exon" => {
                    let transcript = gff_attr(attrs, "Parent")
                        .map(strip_prefix_colon)
                        .unwrap_or_default();
                    self.exons.push(Exon {
                        chrom: chrom.to_string(),
                        start,
                        end,
                        transcript,
                        rank: gff_attr(attrs, "rank").unwrap_or_else(|| NA.to_string()),
                        exon_id: gff_attr(attrs, "exon_id").unwrap_or_else(|| NA.to_string()),
                    });
                }
                _ => {}
            }
        }
    }

    fn load_clinvar(&mut self, text: &str) {
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let c: Vec<&str> = line.split('\t').collect();
            if c.len() < 8 {
                continue;
            }
            let pos: i64 = match c[1].parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            // First ALT allele only (matches the input variant model).
            let alt = c[4].split(',').next().unwrap_or(NA);
            let info = c[7];
            let significance = info_field(info, "CLNSIG")
                .or_else(|| info_field(info, "ONC"))
                .unwrap_or(NA)
                .to_string();
            let oncogenic = info_field(info, "ONC").unwrap_or(NA).to_string();
            // MC is `SO:term|consequence`; take the human-readable term.
            let consequence = info_field(info, "MC")
                .and_then(|mc| mc.split('|').nth(1))
                .unwrap_or(NA)
                .to_string();
            let rsid = info_field(info, "RS")
                .map(|rs| format!("rs{rs}"))
                .unwrap_or_else(|| NA.to_string());
            self.clinvar.insert(
                (c[0].to_string(), pos, c[3].to_string(), alt.to_string()),
                ClinRecord {
                    significance,
                    oncogenic,
                    consequence,
                    id: c[2].to_string(),
                    rsid,
                },
            );
        }
    }

    /// Join `v` against the loaded databases, returning the ordered annotation
    /// columns (see [`ANNOTATION_FIELDS`]). Every column is present; unmatched
    /// columns are `"."`.
    pub fn annotate(&self, v: &Variant) -> Vec<(String, String)> {
        let mut gene = NA.to_string();
        for g in &self.genes {
            if g.chrom == v.chrom && g.start <= v.pos && v.pos <= g.end {
                gene = g.name.clone();
                break;
            }
        }

        // Among exons overlapping the position, pick the one whose parent
        // transcript ranks highest: CCDS first, then longest mRNA span, then
        // lexicographically smallest transcript id (deterministic). This selects
        // the canonical coding transcript (EGFR → ENST00000275493, exon 21).
        let mut best: Option<&Exon> = None;
        for e in &self.exons {
            if e.chrom != v.chrom || e.start > v.pos || v.pos > e.end {
                continue;
            }
            best = Some(match best {
                None => e,
                Some(cur) => {
                    if self.exon_better(e, cur) {
                        e
                    } else {
                        cur
                    }
                }
            });
        }
        let (transcript, exon, exon_id, region) = match best {
            Some(e) => (
                e.transcript.clone(),
                e.rank.clone(),
                e.exon_id.clone(),
                "exon".to_string(),
            ),
            None => {
                // Inside the gene body but no exon ⇒ intronic.
                let region = if gene != NA { "intron" } else { NA };
                (
                    NA.to_string(),
                    NA.to_string(),
                    NA.to_string(),
                    region.to_string(),
                )
            }
        };

        let key = (v.chrom.clone(), v.pos, v.ref_base.clone(), v.alt.clone());
        let clin = self.clinvar.get(&key);
        let sig = clin.map(|c| c.significance.clone());
        // A coordinate hit but ALT-allele miss still leaves consequence from the
        // gene model unknown; consequence comes only from a full ClinVar match.
        let cols = vec![
            ("gene".to_string(), gene),
            ("transcript".to_string(), transcript),
            ("exon".to_string(), exon),
            ("exon_id".to_string(), exon_id),
            ("region".to_string(), region),
            (
                "consequence".to_string(),
                clin.map(|c| c.consequence.clone())
                    .unwrap_or_else(|| NA.to_string()),
            ),
            (
                "clinvar_significance".to_string(),
                sig.unwrap_or_else(|| NA.to_string()),
            ),
            (
                "clinvar_oncogenic".to_string(),
                clin.map(|c| c.oncogenic.clone())
                    .unwrap_or_else(|| NA.to_string()),
            ),
            (
                "clinvar_id".to_string(),
                clin.map(|c| c.id.clone()).unwrap_or_else(|| NA.to_string()),
            ),
            (
                "rsid".to_string(),
                clin.map(|c| c.rsid.clone())
                    .unwrap_or_else(|| NA.to_string()),
            ),
        ];
        cols
    }

    /// Is exon `a`'s transcript a better representative than `b`'s?
    /// CCDS first, then longest span, then smaller transcript id.
    fn exon_better(&self, a: &Exon, b: &Exon) -> bool {
        let ta = self.transcripts.get(&a.transcript);
        let tb = self.transcripts.get(&b.transcript);
        let (a_ccds, a_span) = ta.map(|t| (t.has_ccds, t.span)).unwrap_or((false, 0));
        let (b_ccds, b_span) = tb.map(|t| (t.has_ccds, t.span)).unwrap_or((false, 0));
        match (a_ccds, b_ccds) {
            (true, false) => true,
            (false, true) => false,
            _ => match a_span.cmp(&b_span) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => a.transcript < b.transcript,
            },
        }
    }
}

/// Decode a database buffer to text, transparently inflating BGZF/gzip. BGZF is
/// a series of concatenated gzip blocks, which `MultiGzDecoder` reads as one
/// stream — so this handles both ClinVar `.vcf.gz` and plain `.gff3`.
fn decode_text(bytes: &[u8]) -> Result<String, CoreError> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = String::new();
        flate2::read::MultiGzDecoder::new(bytes)
            .read_to_string(&mut out)
            .map_err(|e| CoreError::BamParse(format!("annotate: gzip inflate: {e}")))?;
        Ok(out)
    } else {
        String::from_utf8(bytes.to_vec())
            .map_err(|e| CoreError::BamParse(format!("annotate: db not utf-8: {e}")))
    }
}

/// Extract a GFF3 attribute value (`;key=value;` in column 9). Returns the raw
/// value (URL-escapes are left as-is; the fields we read carry none).
fn gff_attr(attrs: &str, key: &str) -> Option<String> {
    for kv in attrs.split(';') {
        let kv = kv.trim();
        if let Some(rest) = kv.strip_prefix(key) {
            if let Some(val) = rest.strip_prefix('=') {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// `gene:ENSG…` / `transcript:ENST…` → the part after the colon.
fn strip_prefix_colon(s: String) -> String {
    s.split_once(':').map(|(_, r)| r.to_string()).unwrap_or(s)
}

/// Extract `KEY=value` (or a bare flag `KEY`) from a VCF INFO field.
fn info_field<'a>(info: &'a str, key: &str) -> Option<&'a str> {
    for kv in info.split(';') {
        if let Some(rest) = kv.strip_prefix(key) {
            if let Some(val) = rest.strip_prefix('=') {
                return Some(val);
            }
            if rest.is_empty() {
                return Some(""); // bare flag
            }
        }
    }
    None
}
