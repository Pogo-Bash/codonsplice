#!/usr/bin/env bash
# CNV depth-ratio baseline generator (Track 3 measuring stick).
#
# NA12878 is a GERMLINE NORMAL: across the EGFR region it is copy-number
# diploid (CN=2), so this baseline is the NEGATIVE-CONTROL truth — a flat
# depth ratio (~1.0, log2~0) and ZERO expected CNV events. `CALL cnv` on this
# BAM should therefore call no amplifications/deletions; any call is a false
# positive. The per-window depth-ratio file is the signal Track 3 diffs against.
#
# Coordinates: GRCh37/hg19, contig "7" (matches NA12878_EGFR.bam / chr7.fa).
set -euo pipefail
BAM="${1:-cnvlens/public/sample-data/NA12878_EGFR.bam}"
REGION_CHR=7
REGION_START=54990000
REGION_END=55300000
WIN="${2:-1000}"        # window size (bp)
OUT="${3:-testdata/cnv_depth_baseline.bed}"

WINDOWS="$(mktemp)"
# Build fixed windows across the region (0-based BED).
awk -v c="$REGION_CHR" -v s="$REGION_START" -v e="$REGION_END" -v w="$WIN" \
  'BEGIN{for(p=s;p<e;p+=w){en=p+w; if(en>e)en=e; print c"\t"p"\t"en}}' > "$WINDOWS"

# Mean depth per window via samtools bedcov (sum of per-base depth / width).
TMP="$(mktemp)"
samtools bedcov "$WINDOWS" "$BAM" \
  | awk 'BEGIN{OFS="\t"}{w=$3-$2; md=(w>0)?$4/w:0; print $1,$2,$3,md}' > "$TMP"

# Median window depth (for normalization).
MED=$(awk '{print $4}' "$TMP" | sort -n | awk '{a[NR]=$1} END{ if(NR%2){print a[(NR+1)/2]} else {print (a[NR/2]+a[NR/2+1])/2} }')

# Emit baseline: chrom start end mean_depth depth_ratio log2ratio expected_cn expected_call
{
  echo -e "#chrom\tstart\tend\tmean_depth\tdepth_ratio\tlog2ratio\texpected_cn\texpected_call\t# median_depth=${MED}"
  awk -v m="$MED" 'BEGIN{OFS="\t"}{
    r=(m>0)?$4/m:0;
    l=(r>0)?log(r)/log(2):-99;
    printf "%s\t%s\t%s\t%.3f\t%.3f\t%.3f\t2\tneutral\n",$1,$2,$3,$4,r,l
  }' "$TMP"
} > "$OUT"
rm -f "$WINDOWS" "$TMP"
echo "Wrote $OUT (window=${WIN}bp, median_depth=${MED})"
