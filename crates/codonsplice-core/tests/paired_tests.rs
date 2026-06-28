//! Tumor/normal somatic analysis (`PAIRED WITH`) — end-to-end through the
//! compiler + VM, plus a differential oracle test against `bcftools isec`.
//!
//! The SpliceQL surface is:
//!   `FROM vcf "tumor" PAIRED WITH vcf "normal" [MODE somatic|germline]`
//!
//! `PAIRED WITH` is tumor/normal somatic calling expressed as a VCF set
//! operation: the SOMATIC variants are exactly the records present in TUMOR but
//! ABSENT from NORMAL — i.e. the `private_a` partition (with a=tumor, b=normal)
//! of the already-built `ISEC` set-op. GERMLINE variants are the `shared`
//! partition. This reuses the SAME `cnvlens_core::vcf::isec` function that
//! `ISEC` does — there is no second implementation.
//!
//! The oracle test (gated on `bcftools`/`bgzip`/`tabix`) asserts the somatic
//! set is record-set identical to `bcftools isec -p out tumor.vcf.gz
//! normal.vcf.gz`'s `0000.vcf` (private to tumor), and germline == `0002.vcf`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use codonsplice_core::vm::record_to_json;
use codonsplice_core::{compile, Vm, VmOutput};

/// A variant identity key `(chrom, pos, ref, alt)` — what both our engine and
/// bcftools isec match on.
type Key = (String, i64, String, String);

const HEADER: &str = "\
##fileformat=VCFv4.2
##contig=<ID=7,length=159138663>
##contig=<ID=12,length=133851895>
##INFO=<ID=DP,Number=1,Type=Integer,Description=\"depth\">
#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO
";

// TUMOR: somatic-only 55019021, 55181378; germline (shared) 55241707, 55259515;
// and 55249100/T which is a tumor-only ALT (normal has 55249100/C) → somatic.
const BODY_TUMOR: &str = "\
7\t55019021\t.\tG\tA\t60\tPASS\tDP=100
7\t55181378\t.\tA\tATTG\t50\tPASS\tDP=80
7\t55241707\t.\tG\tT\t70\tPASS\tDP=90
7\t55249100\t.\tA\tT\t55\tPASS\tDP=70
7\t55259515\t.\tT\tG\t99\tPASS\tDP=120
";

// NORMAL: germline (shared) 55241707, 55259515; 55249100/C (diff ALT → not the
// tumor's somatic 55249100/T); plus normal-only 55260000 and 12:25000.
const BODY_NORMAL: &str = "\
7\t55241707\t.\tG\tT\t70\tPASS\tDP=88
7\t55249100\t.\tA\tC\t55\tPASS\tDP=66
7\t55259515\t.\tT\tG\t99\tPASS\tDP=110
7\t55260000\t.\tC\tA\t40\tPASS\tDP=30
12\t25000\t.\tG\tA\t45\tPASS\tDP=50
";

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

/// Run our engine for a given PAIRED-WITH source, returning the result key set.
fn paired_keys(tumor: &str, normal: &str, mode: Option<&str>) -> BTreeSet<Key> {
    let src = match mode {
        Some(m) => format!("FROM vcf \"{tumor}\" PAIRED WITH vcf \"{normal}\" MODE {m}"),
        None => format!("FROM vcf \"{tumor}\" PAIRED WITH vcf \"{normal}\""),
    };
    let program = compile(&src).unwrap_or_else(|e| panic!("compile `{src}`: {e:?}"));
    let recs = match Vm::new(program)
        .run()
        .unwrap_or_else(|e| panic!("run `{src}`: {e:?}"))
    {
        VmOutput::Records(r) | VmOutput::Rows(r) => r,
        other => panic!("expected records, got {other:?}"),
    };
    recs.iter()
        .map(|r| {
            let j = record_to_json(r);
            (
                j["chrom"].as_str().unwrap().to_string(),
                j["pos"].as_i64().unwrap(),
                j["ref"].as_str().unwrap().to_string(),
                j["alt"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Parse a plain `.vcf` file into its key set.
fn vcf_keys(path: &Path) -> BTreeSet<Key> {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            (
                c[0].to_string(),
                c[1].parse().unwrap(),
                c[3].to_string(),
                c[4].to_string(),
            )
        })
        .collect()
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── Pure engine tests (no external tools) ────────────────────────────────────

#[test]
fn paired_default_is_somatic_tumor_private() {
    let dir = unique_dir("paired_engine");
    let t = dir.join("tumor.vcf");
    let n = dir.join("normal.vcf");
    write(&t, &format!("{HEADER}{BODY_TUMOR}"));
    write(&n, &format!("{HEADER}{BODY_NORMAL}"));
    let (ts, ns) = (t.to_str().unwrap(), n.to_str().unwrap());

    // Default (no MODE) == somatic == tumor-private: 55019021, 55181378, 55249100/T.
    let somatic_default = paired_keys(ts, ns, None);
    let somatic_pos: Vec<i64> = somatic_default.iter().map(|k| k.1).collect();
    assert_eq!(
        somatic_pos,
        vec![55019021, 55181378, 55249100],
        "default somatic keys: {somatic_default:?}"
    );
    // The somatic 55249100 is the tumor ALT (T), NOT the normal ALT (C).
    assert!(somatic_default.contains(&("7".into(), 55249100, "A".into(), "T".into())));

    // Explicit MODE somatic == default.
    assert_eq!(somatic_default, paired_keys(ts, ns, Some("somatic")));

    // MODE germline == shared between tumor & normal: 55241707, 55259515.
    let germline = paired_keys(ts, ns, Some("germline"));
    let germ_pos: Vec<i64> = germline.iter().map(|k| k.1).collect();
    assert_eq!(germ_pos, vec![55241707, 55259515], "germline keys: {germline:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Differential oracle: somatic == bcftools isec private-to-tumor (0000.vcf) ──

#[test]
fn somatic_matches_bcftools_isec_private_tumor() {
    if !(have("bcftools") && have("bgzip") && have("tabix")) {
        eprintln!("skipping bcftools oracle: tools not installed");
        return;
    }
    let dir = unique_dir("paired_oracle");
    let t_plain = dir.join("tumor.vcf");
    let n_plain = dir.join("normal.vcf");
    write(&t_plain, &format!("{HEADER}{BODY_TUMOR}"));
    write(&n_plain, &format!("{HEADER}{BODY_NORMAL}"));

    // bgzip + tabix index each input.
    for p in [&t_plain, &n_plain] {
        let s = bash(&format!(
            "bgzip -f {q} && tabix -f -p vcf {q}.gz",
            q = p.display()
        ));
        assert!(s, "bgzip/tabix failed for {}", p.display());
    }
    let t_gz = format!("{}.gz", t_plain.display());
    let n_gz = format!("{}.gz", n_plain.display());

    // bcftools isec -p out tumor.vcf.gz normal.vcf.gz
    let out = dir.join("out");
    let s = bash(&format!(
        "bcftools isec -p {out} {t} {n}",
        out = out.display(),
        t = t_gz,
        n = n_gz
    ));
    assert!(s, "bcftools isec failed");

    // bcftools partitions: 0000 = private to tumor (= SOMATIC),
    //                      0002 = shared, from tumor (= GERMLINE).
    let bt_private_tumor = vcf_keys(&out.join("0000.vcf"));
    let bt_shared = vcf_keys(&out.join("0002.vcf"));

    // Our PAIRED-WITH somatic (default + explicit) and germline, on the SAME
    // bgzipped inputs (BGZF read in-Rust).
    let eng_somatic_default = paired_keys(&t_gz, &n_gz, None);
    let eng_somatic = paired_keys(&t_gz, &n_gz, Some("somatic"));
    let eng_germline = paired_keys(&t_gz, &n_gz, Some("germline"));

    assert_eq!(
        eng_somatic_default, bt_private_tumor,
        "PAIRED WITH somatic (default) must equal bcftools isec 0000.vcf (private to tumor)"
    );
    assert_eq!(
        eng_somatic, bt_private_tumor,
        "PAIRED WITH MODE somatic must equal bcftools isec 0000.vcf (private to tumor)"
    );
    assert_eq!(
        eng_germline, bt_shared,
        "PAIRED WITH MODE germline must equal bcftools isec 0002.vcf (shared)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn bash(cmd: &str) -> bool {
    Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn unique_dir(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut d = std::env::temp_dir();
    d.push(format!("cs_{tag}_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&d).unwrap();
    d
}
