//! VCF set operations (`ISEC`) — end-to-end through the compiler + VM, plus a
//! differential oracle test against `bcftools isec`.
//!
//! The SpliceQL surface is:
//!   `FROM vcf "a" ISEC vcf "b" [MODE shared|shared_b|private_a|private_b|union]`
//!
//! Records are matched on the exact `(chrom, pos, ref, alt)` key. The oracle
//! test (gated on `bcftools`/`bgzip`/`tabix` being installed) asserts our four
//! partitions are *record-set identical* to the files `bcftools isec -p`
//! produces:
//!   * MODE private_a == 0000.vcf
//!   * MODE private_b == 0001.vcf
//!   * MODE shared    == 0002.vcf
//!   * MODE shared_b  == 0003.vcf

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use codonsplice_core::vm::record_to_json;
use codonsplice_core::{compile, Vm, VmOutput};

/// A variant identity key `(chrom, pos, ref, alt)` — what both our engine and
/// bcftools isec match on.
type Key = (String, i64, String, String);

/// Two overlapping VCF bodies (shared header). Designed so every partition is
/// non-empty and includes an exact-match edge case: chr7:55249100 has different
/// ALTs in A (T) and B (C), so it must NOT be counted as shared.
const HEADER: &str = "\
##fileformat=VCFv4.2
##contig=<ID=7,length=159138663>
##contig=<ID=12,length=133851895>
##INFO=<ID=DP,Number=1,Type=Integer,Description=\"depth\">
#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO
";

// A: positions 55019021, 55181378, 55241707(shared), 55249100/T, 55259515(shared)
const BODY_A: &str = "\
7\t55019021\t.\tG\tA\t60\tPASS\tDP=100
7\t55181378\t.\tA\tATTG\t50\tPASS\tDP=80
7\t55241707\t.\tG\tT\t70\tPASS\tDP=90
7\t55249100\t.\tA\tT\t55\tPASS\tDP=70
7\t55259515\t.\tT\tG\t99\tPASS\tDP=120
";

// B: positions 55241707(shared), 55249100/C(diff alt), 55259515(shared), 55260000, 12:25000(private)
const BODY_B: &str = "\
7\t55241707\t.\tG\tT\t70\tPASS\tDP=88
7\t55249100\t.\tA\tC\t55\tPASS\tDP=66
7\t55259515\t.\tT\tG\t99\tPASS\tDP=110
7\t55260000\t.\tC\tA\t40\tPASS\tDP=30
12\t25000\t.\tG\tA\t45\tPASS\tDP=50
";

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
}

/// Run our engine for a given ISEC mode, returning the result key set.
fn engine_keys(a: &str, b: &str, mode: &str) -> BTreeSet<Key> {
    let src = format!("FROM vcf \"{a}\" ISEC vcf \"{b}\" MODE {mode}");
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
fn isec_modes_partition_correctly() {
    let dir = unique_dir("isec_engine");
    let a = dir.join("a.vcf");
    let b = dir.join("b.vcf");
    write(&a, &format!("{HEADER}{BODY_A}"));
    write(&b, &format!("{HEADER}{BODY_B}"));
    let (as_, bs_) = (a.to_str().unwrap(), b.to_str().unwrap());

    // shared: 55241707 and 55259515 (NOT 55249100 — different ALT).
    let shared = engine_keys(as_, bs_, "shared");
    let shared_pos: Vec<i64> = shared.iter().map(|k| k.1).collect();
    assert_eq!(shared_pos, vec![55241707, 55259515], "shared keys: {shared:?}");

    // private_a: 55019021, 55181378, 55249100/T.
    let pa: Vec<i64> = engine_keys(as_, bs_, "private_a")
        .iter()
        .map(|k| k.1)
        .collect();
    assert_eq!(pa, vec![55019021, 55181378, 55249100]);

    // private_b: 55249100/C, 55260000, 12:25000.
    let pb = engine_keys(as_, bs_, "private_b");
    assert!(pb.contains(&("7".into(), 55249100, "A".into(), "C".into())));
    assert!(pb.contains(&("7".into(), 55260000, "C".into(), "A".into())));
    assert!(pb.contains(&("12".into(), 25000, "G".into(), "A".into())));
    assert_eq!(pb.len(), 3);

    // shared_b is the same key set as shared.
    let shared_b = engine_keys(as_, bs_, "shared_b");
    assert_eq!(shared, shared_b, "shared and shared_b match on the same keys");

    // union = private_a ∪ shared ∪ private_b (all distinct keys).
    let union = engine_keys(as_, bs_, "union");
    assert_eq!(union.len(), 3 + 2 + 3, "union covers every distinct key");

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Differential oracle: byte-for-record-set identical to bcftools isec ───────

#[test]
fn matches_bcftools_isec() {
    if !(have("bcftools") && have("bgzip") && have("tabix")) {
        eprintln!("skipping bcftools oracle: tools not installed");
        return;
    }
    let dir = unique_dir("isec_oracle");
    let a_plain = dir.join("a.vcf");
    let b_plain = dir.join("b.vcf");
    write(&a_plain, &format!("{HEADER}{BODY_A}"));
    write(&b_plain, &format!("{HEADER}{BODY_B}"));

    // bgzip (creates a.vcf.gz) + tabix index each input.
    for p in [&a_plain, &b_plain] {
        let s = bash(&format!(
            "bgzip -f {q} && tabix -f -p vcf {q}.gz",
            q = p.display()
        ));
        assert!(s, "bgzip/tabix failed for {}", p.display());
    }
    let a_gz = format!("{}.gz", a_plain.display());
    let b_gz = format!("{}.gz", b_plain.display());

    // bcftools isec -p out a.vcf.gz b.vcf.gz
    let out = dir.join("out");
    let s = bash(&format!(
        "bcftools isec -p {out} {a} {b}",
        out = out.display(),
        a = a_gz,
        b = b_gz
    ));
    assert!(s, "bcftools isec failed");

    // The four bcftools partitions.
    let bt_0000 = vcf_keys(&out.join("0000.vcf")); // private to A
    let bt_0001 = vcf_keys(&out.join("0001.vcf")); // private to B
    let bt_0002 = vcf_keys(&out.join("0002.vcf")); // shared, from A
    let bt_0003 = vcf_keys(&out.join("0003.vcf")); // shared, from B

    // Run our engine against the SAME bgzipped inputs (BGZF read in-Rust).
    let eng_pa = engine_keys(&a_gz, &b_gz, "private_a");
    let eng_pb = engine_keys(&a_gz, &b_gz, "private_b");
    let eng_shared = engine_keys(&a_gz, &b_gz, "shared");
    let eng_shared_b = engine_keys(&a_gz, &b_gz, "shared_b");

    assert_eq!(eng_pa, bt_0000, "private_a must equal bcftools 0000.vcf");
    assert_eq!(eng_pb, bt_0001, "private_b must equal bcftools 0001.vcf");
    assert_eq!(eng_shared, bt_0002, "shared must equal bcftools 0002.vcf");
    assert_eq!(eng_shared_b, bt_0003, "shared_b must equal bcftools 0003.vcf");

    // union = A ∪ B = 0000 ∪ 0002 ∪ 0003 ∪ 0001 (every distinct key bcftools saw).
    let mut bt_union = BTreeSet::new();
    bt_union.extend(bt_0000.iter().cloned());
    bt_union.extend(bt_0001.iter().cloned());
    bt_union.extend(bt_0002.iter().cloned());
    let eng_union = engine_keys(&a_gz, &b_gz, "union");
    assert_eq!(eng_union, bt_union, "union must equal A ∪ B");

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
