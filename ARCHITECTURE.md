# xgrep: Technical Deep Dive

How xgrep searches compressed logs 100-1,200x faster than zgrep by reading less than 1% of the data.

## What xgrep is

xgrep is an indexed search accelerator for compressed log files. It builds a one-time sidecar index alongside your `.gz` files, then uses bloom filters and memory-mapped I/O to touch only the blocks that might contain your search term. Everything else stays on disk.

**When it wins:** Repeated searches over the same compressed log corpus — incident investigation, debugging sessions, log forensics. The index build is a one-time cost; every subsequent query benefits.

**What "cached" means:** All benchmark results labeled "cached" or "repeated query" assume the index has already been built via `xgrep --build-index`. First-query performance (no cache) is reported separately. This is not a limitation — it is the design. Engineers investigating an incident run dozens of queries against the same logs. The first query pays the index cost; the rest are near-instant.

**Known limitation:** Lines spanning a 64KB block boundary are attributed to the block where they start. This is conservative (no false negatives) and affects less than 0.1% of lines in practice.

## Benchmark summary

Full methodology is in the [Benchmark methodology](#benchmark-methodology) section below. All datasets are from [LogHub](https://github.com/logpai/loghub) — real production logs.

| Dataset | Files | Compressed | Decompressed | Query | xgrep (cached) | zgrep | Speedup |
|---|---|---|---|---|---|---|---|
| HDFS | 76 | 743MB | 7.5GB | Specific block ID | 30ms | 27.4s | **913x** |
| HDFS | 76 | 743MB | 7.5GB | `INFO` (broad) | 23ms | 28.0s | **1,217x** |
| BGL | 50 | 421MB | 5.0GB | Specific node | 26ms | 17.4s | **655x** |
| BGL | 50 | 421MB | 5.0GB | `FATAL` (rare) | 25ms | 17.4s | **708x** |
| Spark | 3,852 | 189MB | 2.8GB | `Executor` | 5.1s | 10m 2s | **118x** |
| Spark | 3,852 | 189MB | 2.8GB | `ERROR` | 2.7s | ~10m | **220x** |

Key metric: **bytes touched per query is ~0.1-1% of corpus vs 100% for zgrep.**

### First query vs repeated query

| Mode | Time | Notes |
|---|---|---|
| `xgrep --build-index` | ~2 min | One-time. Decompresses, builds blooms, writes cache. |
| xgrep first query (no cache) | 2.4s | Parallel decompress + scan. Already 18x faster than zgrep. |
| xgrep repeated query (cached) | 11-30ms | Bloom skip + mmap. Only candidate blocks paged in. |
| zgrep | 44s | Full decompress + scan. Every time. |

## Why not just use...

| Tool | What it does | Why xgrep is different |
|---|---|---|
| **zgrep** | Decompresses entire file, scans every byte, every query | xgrep caches decompressed data + bloom index, touches only candidate blocks |
| **ripgrep** | Fast text search, no compressed file support | xgrep handles `.gz` natively and adds block-level skip via bloom filters |
| **jq** | JSON processor, evaluates every line | xgrep's JSON mode (`-j`) skips blocks using bloom-indexed field-value pairs |
| **Elasticsearch / Splunk** | Full log platforms with ingest pipelines | xgrep is a single binary with no infrastructure — `cargo install` and search |

## Architecture overview

```
                    Traditional (zgrep)
                    ==================
                    decompress ALL -> scan ALL -> output matches
                    Cost: O(corpus) per query

                    xgrep (cached)
                    ==============
    query
      |
    check bloom filters (nanoseconds per block)
      |
    identify candidate blocks (~1% of total)
      |
    mmap: OS pages in only those blocks
      |
    match + output
    Cost: O(candidates) per query
```

The key insight: **xgrep's cost scales with the number of candidate blocks, not the corpus size.** A 7.5GB corpus with a selective query touches ~75MB. A 75GB corpus with the same query still touches ~75MB.

## Index structure

### Building the index

When you run `xgrep --build-index logs/*.gz`, xgrep:

1. Decompresses each `.gz` file
2. Divides the decompressed content into 64KB blocks
3. For each block, builds a token-level bloom filter
4. Writes two sidecar files:
   - `index.xgi` — consolidated bloom filters for all files
   - `index.xgd` — concatenated decompressed content

```
logs/
  app-001.log.gz     (compressed source -- untouched)
  app-002.log.gz
  .xgrep/
    index.xgi        (bloom filters -- ~6% of decompressed size)
    index.xgd        (decompressed content -- memory-mapped)
```

### Consolidated index format

The `.xgi` file contains a header followed by per-file entries:

```
[8 bytes]   magic: "XGREP02\0"
[4 bytes]   file_count
[4 bytes]   block_size (64KB)
[4 bytes]   bloom_size (4096 bytes)
[4 bytes]   reserved

Per file:
  [4 bytes]   path_len
  [N bytes]   filename
  [8 bytes]   source_size (staleness check)
  [8 bytes]   source_mtime_ns (staleness check)
  [8 bytes]   data_offset into .xgd
  [8 bytes]   data_len
  [4 bytes]   block_count
  [block_count * 4096 bytes]  bloom filters
```

A single consolidated index replaces N per-file index opens with 2 file opens (one index, one data). This matters when searching thousands of files.

### Staleness

Every query checks the source file's size and mtime against the cached values. If the source has changed, the cached entry is ignored and xgrep falls back to direct search. No manual cache invalidation needed.

## Bloom filter design

### Why bloom filters

A bloom filter is a compact probabilistic data structure that can tell you "this term is definitely NOT in this block" or "this term MIGHT be in this block." The first case lets xgrep skip the block entirely. The second case means it searches the block (with a small false positive rate).

### Token-level indexing

xgrep splits each 64KB block into tokens using a set of delimiters:

```
space, tab, newline, =, :, /, -, _, [, ], (, ), {, }, ", ', comma, ;, |, <, >, @, #, &
```

This means a log line like:

```
2026-01-15 10:23:45 ERROR user_id=12345 req=req-00a1b2 /api/users status=503
```

produces tokens: `2026`, `01`, `15`, `10`, `23`, `45`, `ERROR`, `user`, `id`, `12345`, `req`, `00a1b2`, `api`, `users`, `status`, `503`

Each token (lowercased, length >= 2) is inserted into the block's bloom filter.

### Why not trigrams

The initial design used trigram indexing (every 3-byte subsequence). This saturated the bloom filter: a 64KB block produces ~20,000 unique trigrams, filling a 512-bit filter to 100%. Every bit is set, so the filter can never reject anything.

Token-level indexing produces ~2,000-4,000 unique tokens per 64KB block. With a 4KB (32,768-bit) bloom filter and 3 hash functions, this gives a ~3% false positive rate — low enough to skip 95-99% of non-matching blocks.

### Parameters

| Parameter | Value | Rationale |
|---|---|---|
| Block size | 64KB | Balances granularity vs overhead. Smaller = more blocks to check. Larger = less precise skipping. |
| Bloom size | 4KB (32,768 bits) | 3% FP at 4,000 tokens. 6.25% overhead ratio (4KB bloom per 64KB data). |
| Hash functions | 3 (FNV-1a + double hashing) | Standard for this filter size. Matches HPLOG's proven design. |
| Min token length | 2 | Single-character tokens produce too many false positives. |

### Hash function

```
FNV-1a base hash:
  h = 0xcbf29ce484222325 (FNV offset basis)
  for each byte b in token:
    h ^= b
    h *= 0x100000001b3 (FNV prime)

Position derivation (double hashing + quadratic probe):
  bit_i = h1 + i*h2 + i*i
  where h1 = lower 32 bits, h2 = upper 32 bits
```

This generates 3 independent bit positions from a single hash computation.

## Memory-mapped search

### How mmap enables the speedup

The decompressed cache (`index.xgd`) is memory-mapped, not read into memory. When xgrep identifies that blocks 47, 312, and 1089 are candidates out of 120,000 total blocks, it accesses only those byte ranges. The operating system's virtual memory system pages in only the 4KB pages that are actually touched.

On a 7.5GB cache file with 98% skip rate:
- **Without mmap**: `fs::read` loads all 7.5GB into RAM (~200ms just for I/O)
- **With mmap**: OS pages in ~150MB of candidate blocks (~5ms)

This is why cached queries complete in 20-30ms regardless of corpus size.

### The count-only fast path

For `-c` (count), `-l` (list files), and `-q` (quiet) modes, xgrep skips line string allocation entirely. It iterates only over candidate blocks, finds newlines with SIMD (memchr), and runs the matcher without constructing owned strings for non-matching lines.

## Query pipeline

For each query, xgrep follows this pipeline:

```
1. Parse pattern
   +-- Fixed string (-F): extract literal bytes
   +-- Regex: extract longest required literal substring
   +-- JSON (-j): parse field=value clauses

2. Check for cached index
   +-- Consolidated index exists? -> use it (2 file opens)
   +-- Per-file index exists? -> use it
   +-- No cache -> fall back to direct search

3. Bloom prefilter (if cache exists)
   For each block's bloom filter:
   +-- Query tokens present? -> candidate (search this block)
   +-- Any token absent? -> skip (definitely no match)

4. Search only candidate blocks
   +-- mmap: access only candidate block byte ranges
   +-- Line splitting via memchr SIMD
   +-- Match via memchr (fixed string) or regex crate
   +-- Early exit for -q, -m modes

5. Output
   +-- Grep-compatible formatting with optional color
```

### Literal extraction for regex

For regex patterns, xgrep extracts the longest required literal substring to use as a bloom prefilter. For example:

```
Pattern: "ERROR.*timeout"
Extracted literal: "timeout" (7 bytes, longer than "ERROR")

Pattern: "status=50[0-3]"
Extracted literal: "status=50" (9 bytes)
```

The bloom filter checks for the extracted literal. Only blocks where the literal might exist proceed to full regex evaluation.

## Uncached search path

When no index exists, xgrep still provides value through:

1. **Parallel file search** (rayon) — all files searched concurrently
2. **SIMD literal precheck** — each 64KB block is scanned for the literal using `memchr::memmem` before running the full matcher. This avoids regex evaluation on blocks that don't contain the literal.

This path is typically 3-12x faster than zgrep on multi-file workloads due to parallelism alone.

## JSON mode

### Field-value bloom indexing

In JSON mode (`-j`), xgrep parses each NDJSON line and inserts three entries per field-value pair:

1. Field name alone (enables field-existence queries)
2. Value alone (maintains text-mode compatibility)
3. `field\0value` concatenation (the discriminator — binds field to specific value)

The null byte separator prevents false collisions between field names and values.

### Query evaluation

A query like `level=error status=500` is parsed into two filter clauses. A block is skipped only if ANY clause's `field\0value` hash is absent from the bloom. This provides AND semantics at the bloom level — the final line-by-line match confirms exact JSON field equality.

## Benchmark methodology

### Datasets

All benchmarks use publicly available datasets from [LogHub](https://github.com/logpai/loghub) (Zhu et al., 2023):

| Dataset | Source | Lines (raw) | Scaling | Final lines | Compressed | Decompressed |
|---|---|---|---|---|---|---|
| HDFS | Hadoop cluster | 11.2M | 5x concat | 55.8M | 743MB | 7.5GB |
| BGL | Blue Gene/L supercomputer | 4.7M | 7x concat | 33.2M | 421MB | 5.0GB |
| Spark | Distributed compute cluster | 33.2M | 1x (as-is) | 33.2M | 189MB | 2.8GB |

Datasets were concatenated to reach multi-GB scale, then split into chunks and gzip-compressed to simulate rotated log archives.

### Procedure

1. Build consolidated index: `xgrep --build-index <dir>/*.gz`
2. Run xgrep cached query (best of 3 runs): `xgrep -c -F '<term>' <dir>/*.gz`
3. Run zgrep baseline (sequential loop): `for f in <dir>/*.gz; do zgrep -c -F '<term>' "$f"; done`
4. zgrep is run sequentially because it does not parallelize — this is the real-world baseline

### Why zgrep is sequential

zgrep processes one file at a time. This is how engineers actually use it (`zgrep pattern *.gz`). While GNU parallel could speed up zgrep, that changes the tool — the comparison is against `zgrep` as engineers encounter it, not a hypothetical parallelized wrapper.

### Cache state

- Index build time is reported separately from query time
- Cached queries benefit from OS page cache on the mmap'd data file after the first run
- First cached query (cold mmap) and subsequent queries (warm mmap) are within 2x of each other; both are reported where measured

### Hardware

Benchmarks were run on a single Windows 11 workstation. Results will vary by disk speed, CPU, and available memory. The relative ratios are more meaningful than absolute times.

## Results

### HDFS — 76 files, 743MB gz, 7.5GB decompressed

| Query | Selectivity | xgrep (cached) | zgrep | Speedup |
|---|---|---|---|---|
| `blk_-1608999687919862906` | Specific block ID | 30ms | 27.4s | 913x |
| `WARN` | ~3% of lines | 28ms | 25.4s | 907x |
| `INFO` | ~97% of lines | 23ms | 28.0s | 1,217x |

### BGL — 50 files, 421MB gz, 5.0GB decompressed

| Query | Selectivity | xgrep (cached) | zgrep | Speedup |
|---|---|---|---|---|
| `R02-M1-N0` | Specific node | 26ms | 17.4s | 655x |
| `FATAL` | Rare | 25ms | 17.4s | 708x |
| `INFO` | Broad | 27ms | 17.7s | 655x |

### Spark — 3,852 files, 189MB gz, 2.8GB decompressed

| Query | Selectivity | xgrep (cached) | zgrep | Speedup |
|---|---|---|---|---|
| `Executor` | Medium | 5.1s | 10m 2s | 118x |
| `ERROR` | Broad | 2.7s | ~10m | 220x |

Spark's lower ratios reflect 3,852-file overhead in the consolidated index, not a bloom filter limitation.

## Limitations

### Cache storage

The decompressed cache is ~5-6x the compressed size. A 1.7GB compressed corpus requires ~9GB of cache. This is the tradeoff for avoiding repeated decompression.

### First query cost

Building the index requires full decompression. On a 1.7GB corpus, this takes ~2.5 minutes. This is amortized over all subsequent queries.

### Broad queries with low selectivity

When a search term appears in every block, bloom filters cannot skip anything. xgrep still wins because it avoids decompression (using the cached data), but the advantage comes from mmap rather than bloom skipping.

### Single-stream gzip

Standard gzip files are a single deflate stream — you cannot seek to the middle without decompressing from the start. xgrep works around this by caching the decompressed content. A future `xgrep pack` command could recompress into independently seekable blocks, eliminating the cache size overhead.

### Line boundaries at block edges

Lines that span a 64KB block boundary are attributed to the block where they start. A match in such a line might cause the block to be searched even if the bloom filter for that block alone would have skipped it. This is conservative (no false negatives) and affects less than 0.1% of lines in practice.

## References

- Zhu, J., et al. "Loghub: A Large Collection of System Log Datasets for AI-driven Log Analytics." IEEE International Symposium on Software Reliability Engineering (ISSRE), 2023. [github.com/logpai/loghub](https://github.com/logpai/loghub)
- Bloom, B. H. "Space/time trade-offs in hash coding with allowable errors." Communications of the ACM, 1970.
- Galil, Z. "On improving the worst case running time of the Boyer-Moore string matching algorithm." Communications of the ACM, 1979.
