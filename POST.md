# Your logs are still a text file

Every incident investigation starts the same way. Something is broken. You have compressed logs. You run zgrep.

```
zgrep "user_id=51013" logs/*.gz
```

And you wait. 30 seconds. A minute. You refine the query, run it again. Another minute. You try a different field, a different time range. Each query costs the same: full decompression, full scan, every byte, every time.

After ten queries you've spent ten minutes waiting for the same data to be decompressed and scanned ten times. The files haven't changed. The decompression work is identical. But zgrep doesn't know that.

## What if grep could remember?

I built xgrep to solve this. It works like this:

```bash
# One-time: build an index (~2 min for 1.7GB compressed)
xgrep --build-index logs/*.gz

# Every query after that
xgrep "user_id=51013" logs/*.gz    # 25ms
xgrep "ERROR" logs/*.gz            # 25ms
xgrep "timeout.*conn" logs/*.gz    # 25ms
```

The first time, xgrep decompresses your logs, splits them into 64KB blocks, and builds a compact bloom filter for each block. On every subsequent query, it checks those filters to figure out which blocks *might* contain your search term — and only reads those blocks. The rest never leave disk.

On real production logs, this means touching about **1% of the data instead of 100%**.

## The numbers

I tested on three real-world datasets from [LogHub](https://github.com/logpai/loghub) — actual production logs from Hadoop, a Blue Gene/L supercomputer, and Spark.

### HDFS (Hadoop) — 76 files, 743MB compressed, 7.5GB decompressed

| Query | xgrep | zgrep | Speedup |
|---|---|---|---|
| Specific block ID | 30ms | 27s | **913x** |
| `WARN` | 28ms | 25s | **907x** |
| `INFO` (appears in almost every line) | 23ms | 28s | **1,217x** |

### BGL (Supercomputer) — 50 files, 421MB compressed, 5.0GB decompressed

| Query | xgrep | zgrep | Speedup |
|---|---|---|---|
| Specific node ID | 26ms | 17s | **655x** |
| `FATAL` | 25ms | 17s | **708x** |

### Spark — 3,852 files, 189MB compressed

| Query | xgrep | zgrep | Speedup |
|---|---|---|---|
| `Executor` | 5s | 10 min | **118x** |
| `ERROR` | 2.7s | 10 min | **220x** |

These are cached, repeated-query numbers. The first query without a cache is still 18x faster than zgrep (parallel decompression), but the real value is the second query and beyond — which is exactly what incident investigation looks like.

## How it works (short version)

1. **Build index**: decompress each `.gz`, divide into 64KB blocks, build a 4KB bloom filter per block that records which tokens appear in that block. Write everything to a sidecar `.xgrep/` directory.

2. **Query**: load the bloom filters (small), check each block's filter against your search terms (nanoseconds per block), memory-map the cached data, and let the OS page in only the candidate blocks. A 7.5GB file where 98% of blocks are skipped means the OS reads ~150MB.

3. **Output**: grep-compatible. Same flags you already know: `-n`, `-c`, `-l`, `-i`, `-F`, `-A/-B/-C`, color.

The key metric isn't speed — it's **bytes touched per query**. zgrep touches 100% of the corpus every time. xgrep touches 0.1-1%. That's why the ratio gets better as your logs get bigger.

For the full architecture and benchmark methodology: [ARCHITECTURE.md](ARCHITECTURE.md)

## Tradeoffs (honest)

- **Cache size**: the decompressed data is stored alongside the `.gz` files. Roughly 5x the compressed size. This is the cost of avoiding repeated decompression.
- **First query**: building the index takes ~2 minutes for a 1.7GB corpus. Amortized over dozens of queries during an investigation, this pays for itself immediately.
- **Not a universal grep replacement**: xgrep is built for searching compressed log archives repeatedly. For one-off searches on plain text, use ripgrep.

## Who this is for

If you've ever:
- Waited on `zgrep` during an incident
- Run the same search ten times while narrowing a bug
- Wished rotated `.gz` logs were searchable without a log platform
- Wanted Elasticsearch-level speed without Elasticsearch-level infrastructure

```bash
cargo install --path .
# or
cargo build --release
```

[github.com/HighpassStudio/xgrep](https://github.com/HighpassStudio/xgrep)

---

*xgrep is Apache-2.0 licensed. Built with Rust, rayon, memchr, and flate2.*
