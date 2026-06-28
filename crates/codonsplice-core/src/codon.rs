//! The single shared codon / genetic-code module.
//!
//! Everything that needs the genetic code or a reference-derived codon goes
//! through here: the SpliceQL builtins `translate()`, `gc()`, `codon_at()`, and
//! the HGVS/protein annotation emitted by [`crate::annotate`]. There is exactly
//! **one** NCBI translation table ([`codon_to_aa`]) and **one** codon extractor
//! ([`codon_at_genomic`]) — no forks.
//!
//! Genomic coordinates are 1-based inclusive (VCF/GFF convention). A reference
//! is supplied as a closure `pos -> base`, so callers can back it by an
//! absolute-position FASTA string ([`crate::vm`]'s `parse_fasta`) or a synthetic
//! map in tests, without this module owning any I/O.

/// DNA strand of a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strand {
    Plus,
    Minus,
}

/// One CDS segment of a transcript, genomic 1-based inclusive, with its GFF3
/// `phase` (bases to trim from the 5' end of this segment to reach the first
/// complete codon). Only the *first* segment's phase is consulted, to support
/// 5'-truncated CDS; downstream segments are reached by cumulative length.
#[derive(Debug, Clone)]
pub struct CdsSegment {
    pub start: i64,
    pub end: i64,
    pub phase: u8,
}

/// A transcript's coding model: strand plus its CDS segments. Segments may be
/// supplied in any order; [`CdsModel::new`] sorts them into genomic-ascending
/// order and remembers the strand so coding offsets run 5'→3'.
#[derive(Debug, Clone)]
pub struct CdsModel {
    pub strand: Strand,
    /// CDS segments, genomic-ascending.
    pub segments: Vec<CdsSegment>,
}

/// The codon overlapping a queried genomic position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodonHit {
    /// The reference codon on the **coding** strand, 5'→3' (already
    /// reverse-complemented for minus-strand transcripts), e.g. `"CTG"`.
    pub codon: String,
    /// 0-based codon number along the CDS; the amino-acid number is
    /// `codon_index + 1` (codon 0 ⇒ residue 1).
    pub codon_index: usize,
    /// Position (0/1/2) of the queried base **within its codon**, in coding
    /// orientation.
    pub frame: usize,
    /// 1-based coding-sequence position (the `c.` coordinate) of the queried
    /// base.
    pub cds_pos: usize,
}

impl CdsModel {
    /// Build a model, sorting `segments` genomic-ascending.
    pub fn new(strand: Strand, mut segments: Vec<CdsSegment>) -> Self {
        segments.sort_by_key(|s| s.start);
        CdsModel { strand, segments }
    }

    /// Total coding length (sum of segment lengths).
    pub fn coding_len(&self) -> i64 {
        self.segments.iter().map(|s| s.end - s.start + 1).sum()
    }

    /// 0-based coding offset (5'→3') of genomic position `pos`, or `None` if
    /// `pos` is not inside any CDS segment.
    fn genomic_to_coding(&self, pos: i64) -> Option<i64> {
        match self.strand {
            Strand::Plus => {
                let mut cum = 0;
                for s in &self.segments {
                    if pos >= s.start && pos <= s.end {
                        return Some(cum + (pos - s.start));
                    }
                    cum += s.end - s.start + 1;
                }
                None
            }
            Strand::Minus => {
                let mut cum = 0;
                for s in self.segments.iter().rev() {
                    if pos >= s.start && pos <= s.end {
                        return Some(cum + (s.end - pos));
                    }
                    cum += s.end - s.start + 1;
                }
                None
            }
        }
    }

    /// Genomic position of 0-based coding offset `off`, or `None` if past the
    /// CDS end.
    fn coding_to_genomic(&self, off: i64) -> Option<i64> {
        if off < 0 {
            return None;
        }
        match self.strand {
            Strand::Plus => {
                let mut cum = 0;
                for s in &self.segments {
                    let len = s.end - s.start + 1;
                    if off < cum + len {
                        return Some(s.start + (off - cum));
                    }
                    cum += len;
                }
                None
            }
            Strand::Minus => {
                let mut cum = 0;
                for s in self.segments.iter().rev() {
                    let len = s.end - s.start + 1;
                    if off < cum + len {
                        return Some(s.end - (off - cum));
                    }
                    cum += len;
                }
                None
            }
        }
    }
}

/// Extract the reference codon overlapping genomic `pos` for transcript `cds`.
///
/// `ref_base_at(p)` returns the reference base (any case) at 1-based genomic
/// position `p`, or `None` if unavailable. Codons that span an exon boundary are
/// reconstructed correctly because each codon base is mapped back through the
/// CDS model independently. Minus-strand codons are reverse-complemented to the
/// coding strand. Returns `None` if `pos` is not in the CDS, or any codon base
/// is missing from the reference.
pub fn codon_at_genomic(
    cds: &CdsModel,
    pos: i64,
    ref_base_at: impl Fn(i64) -> Option<u8>,
) -> Option<CodonHit> {
    let off = cds.genomic_to_coding(pos)?;
    let frame = (off % 3) as usize;
    let codon_index = (off / 3) as usize;
    let codon_start = (codon_index as i64) * 3;

    let mut codon = String::with_capacity(3);
    for k in 0..3 {
        let gpos = cds.coding_to_genomic(codon_start + k)?;
        let mut base = ref_base_at(gpos)?.to_ascii_uppercase();
        if cds.strand == Strand::Minus {
            base = complement_base(base);
        }
        codon.push(base as char);
    }

    Some(CodonHit {
        codon,
        codon_index,
        frame,
        cds_pos: (off + 1) as usize,
    })
}

/// Map a base to its index in T,C,A,G order (the layout of the codon table).
fn base_idx(b: u8) -> Option<usize> {
    match b.to_ascii_uppercase() {
        b'T' | b'U' => Some(0),
        b'C' => Some(1),
        b'A' => Some(2),
        b'G' => Some(3),
        _ => None,
    }
}

/// Single-letter amino acid for a DNA codon (NCBI translation table 1). `*` is a
/// stop codon; `X` is returned for any codon containing a non-ACGT base. This is
/// the project's single genetic-code table.
pub fn codon_to_aa(codon: &[u8]) -> char {
    // Amino acids indexed by base1*16 + base2*4 + base3, each base in T,C,A,G order.
    const AAS: &[u8] = b"FFLLSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG";
    if codon.len() < 3 {
        return 'X';
    }
    match (base_idx(codon[0]), base_idx(codon[1]), base_idx(codon[2])) {
        (Some(a), Some(b), Some(d)) => AAS[a * 16 + b * 4 + d] as char,
        _ => 'X',
    }
}

/// Translate DNA to a single-letter amino-acid string starting at `frame`
/// (0/1/2); a trailing partial codon is dropped.
pub fn translate(dna: &str, frame: usize) -> String {
    let bases: Vec<u8> = dna.bytes().collect();
    let mut aa = String::new();
    let mut i = frame;
    while i + 3 <= bases.len() {
        aa.push(codon_to_aa(&bases[i..i + 3]));
        i += 3;
    }
    aa
}

/// Fraction of G/C among A/C/G/T bases (other symbols ignored); 0.0 if none.
pub fn gc(seq: &str) -> f64 {
    let (mut gc, mut at) = (0u64, 0u64);
    for b in seq.bytes() {
        match b.to_ascii_uppercase() {
            b'G' | b'C' => gc += 1,
            b'A' | b'T' => at += 1,
            _ => {}
        }
    }
    let denom = gc + at;
    if denom == 0 {
        0.0
    } else {
        gc as f64 / denom as f64
    }
}

/// Complement a single DNA base (uppercased; N/unknown pass through).
fn complement_base(b: u8) -> u8 {
    match b.to_ascii_uppercase() {
        b'A' => b'T',
        b'T' | b'U' => b'A',
        b'C' => b'G',
        b'G' => b'C',
        other => other,
    }
}

/// Reverse complement of a DNA string (uppercased; N/unknown pass through).
pub fn revcomp(seq: &str) -> String {
    seq.bytes()
        .rev()
        .map(|b| complement_base(b) as char)
        .collect()
}

/// The codon (3 chars) starting at char index `i` in `seq`, or `None` if it
/// would run past the end. Powers the string-form `codon_at(seq, i)` builtin.
pub fn codon_at_index(seq: &str, i: usize) -> Option<String> {
    let chars: Vec<char> = seq.chars().collect();
    if i + 3 > chars.len() {
        None
    } else {
        Some(chars[i..i + 3].iter().collect())
    }
}

/// 1-letter amino-acid code → 3-letter HGVS code; `*` → `Ter`, `X` → `Xaa`.
pub fn aa1_to_aa3(c: char) -> &'static str {
    match c.to_ascii_uppercase() {
        'A' => "Ala",
        'R' => "Arg",
        'N' => "Asn",
        'D' => "Asp",
        'C' => "Cys",
        'Q' => "Gln",
        'E' => "Glu",
        'G' => "Gly",
        'H' => "His",
        'I' => "Ile",
        'L' => "Leu",
        'K' => "Lys",
        'M' => "Met",
        'F' => "Phe",
        'P' => "Pro",
        'S' => "Ser",
        'T' => "Thr",
        'W' => "Trp",
        'Y' => "Tyr",
        'V' => "Val",
        '*' => "Ter",
        _ => "Xaa",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── genetic code ─────────────────────────────────────────────────────
    #[test]
    fn genetic_code_table() {
        assert_eq!(codon_to_aa(b"CTG"), 'L'); // Leu
        assert_eq!(codon_to_aa(b"CGG"), 'R'); // Arg
        assert_eq!(codon_to_aa(b"ATG"), 'M'); // Met / start
        assert_eq!(codon_to_aa(b"TAA"), '*'); // stop (ochre)
        assert_eq!(codon_to_aa(b"TAG"), '*'); // stop (amber)
        assert_eq!(codon_to_aa(b"TGA"), '*'); // stop (opal)
        assert_eq!(codon_to_aa(b"TTT"), 'F'); // Phe
        assert_eq!(codon_to_aa(b"GGG"), 'G'); // Gly
        assert_eq!(codon_to_aa(b"NNN"), 'X'); // ambiguous
    }

    #[test]
    fn translate_frames() {
        assert_eq!(translate("ATGAAATAG", 0), "MK*");
        assert_eq!(translate("ATGTTT", 0), "MF");
        // frame 1 drops the leading A: TGT TT -> "C" (+ trailing partial dropped)
        assert_eq!(translate("ATGTTT", 1), "C");
    }

    #[test]
    fn gc_fraction() {
        assert_eq!(gc("GGCC"), 1.0);
        assert_eq!(gc("ATAT"), 0.0);
        assert_eq!(gc("ATGC"), 0.5);
        assert_eq!(gc("NNNN"), 0.0); // no ACGT → 0, not NaN
    }

    #[test]
    fn revcomp_and_codon_index() {
        assert_eq!(revcomp("AACG"), "CGTT");
        assert_eq!(codon_at_index("ATGAAA", 3).as_deref(), Some("AAA"));
        assert_eq!(codon_at_index("ATG", 2), None);
    }

    #[test]
    fn aa_three_letter() {
        assert_eq!(aa1_to_aa3('L'), "Leu");
        assert_eq!(aa1_to_aa3('R'), "Arg");
        assert_eq!(aa1_to_aa3('*'), "Ter");
    }

    // ── codon extraction from a reference, plus strand ───────────────────
    //
    // Synthetic single-segment CDS, phase 0, plus strand. Coding sequence
    // "ATG CTG AAA" laid at genomic 101..=109.
    fn plus_ref(p: i64) -> Option<u8> {
        let seq = b"ATGCTGAAA"; // genomic 101..109
        let idx = p - 101;
        if (0..seq.len() as i64).contains(&idx) {
            Some(seq[idx as usize])
        } else {
            None
        }
    }

    #[test]
    fn codon_extraction_plus_strand() {
        let cds = CdsModel::new(
            Strand::Plus,
            vec![CdsSegment { start: 101, end: 109, phase: 0 }],
        );
        // pos 104 = 'C', first base of codon 2 ("CTG"), residue 2, frame 0.
        let hit = codon_at_genomic(&cds, 104, plus_ref).unwrap();
        assert_eq!(hit.codon, "CTG");
        assert_eq!(hit.codon_index, 1); // residue 2
        assert_eq!(hit.frame, 0);
        assert_eq!(hit.cds_pos, 4); // c.4
        // pos 105 = middle of CTG, frame 1, c.5
        let hit = codon_at_genomic(&cds, 105, plus_ref).unwrap();
        assert_eq!(hit.codon, "CTG");
        assert_eq!(hit.frame, 1);
        assert_eq!(hit.cds_pos, 5);
    }

    // ── codon extraction, minus strand ───────────────────────────────────
    //
    // Coding sequence "ATGCTG" on the minus strand. Coding base 1 (A) sits at
    // the highest genomic coordinate; the template-strand bases are the
    // complements. Lay the template strand at genomic 201..=206 as the
    // complement of the reverse of the coding seq.
    //   coding 5'->3':      A T G C T G
    //   genomic (template), 5'->3' on + strand = revcomp(coding) = C A G C A T
    //   at genomic 201..206:  C A G C A T
    fn minus_ref(p: i64) -> Option<u8> {
        let seq = b"CAGCAT"; // + strand genomic 201..206
        let idx = p - 201;
        if (0..seq.len() as i64).contains(&idx) {
            Some(seq[idx as usize])
        } else {
            None
        }
    }

    #[test]
    fn codon_extraction_minus_strand() {
        let cds = CdsModel::new(
            Strand::Minus,
            vec![CdsSegment { start: 201, end: 206, phase: 0 }],
        );
        // Coding offset 0 (residue 1, "ATG") is at the highest genomic pos 206.
        let hit = codon_at_genomic(&cds, 206, minus_ref).unwrap();
        assert_eq!(hit.codon, "ATG");
        assert_eq!(hit.codon_index, 0);
        assert_eq!(hit.frame, 0);
        // Coding offset 3 ("CTG", residue 2) starts at genomic pos 203.
        let hit = codon_at_genomic(&cds, 203, minus_ref).unwrap();
        assert_eq!(hit.codon, "CTG");
        assert_eq!(hit.codon_index, 1);
    }

    // ── codon spanning an exon boundary ──────────────────────────────────
    #[test]
    fn codon_spans_exon_boundary() {
        // Two CDS segments: 301..302 ("AT") and 401..405 ("GCTGA"); coding seq
        // = "ATGCTGA". Codon 1 = "ATG" spans the 302/401 boundary.
        fn r(p: i64) -> Option<u8> {
            match p {
                301 => Some(b'A'),
                302 => Some(b'T'),
                401 => Some(b'G'),
                402 => Some(b'C'),
                403 => Some(b'T'),
                404 => Some(b'G'),
                405 => Some(b'A'),
                _ => None,
            }
        }
        let cds = CdsModel::new(
            Strand::Plus,
            vec![
                CdsSegment { start: 301, end: 302, phase: 0 },
                CdsSegment { start: 401, end: 405, phase: 2 },
            ],
        );
        // pos 401 is the 3rd base of codon 1 ("ATG"); frame 2, c.3.
        let hit = codon_at_genomic(&cds, 401, r).unwrap();
        assert_eq!(hit.codon, "ATG");
        assert_eq!(hit.codon_index, 0);
        assert_eq!(hit.frame, 2);
        assert_eq!(hit.cds_pos, 3);
    }
}
