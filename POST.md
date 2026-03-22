# Your logs are still a text file

Every incident investigation starts the same way:

```
zgrep "user_id=51013" logs/*.gz
```

...and you wait.

30 seconds. A minute.

You tweak the query. Run it again. Another minute.

Same files. Same decompression. Same full scan.

After ten queries, you've spent ten minutes re-reading the same data.

## What if grep could remember?

I built xgrep for that.

```bash
# One-time: build index (~2 min for 1.7GB)
xgrep --build-index logs/*.gz

# Every query after that
xgrep "user_id=51013" logs/*.gz    # 25ms
xgrep "ERROR" logs/*.gz            # 25ms
xgrep "timeout.*conn" logs/*.gz    # 25ms
```

Instead of decompressing everything every time, xgrep:

- splits logs into 64KB blocks
- builds a bloom filter per block
- only reads blocks that might match

Everything else is skipped.

**The result: read 1% of the data instead of 100%.**

That's the whole idea.

## Benchmarks on real production logs

Datasets from [LogHub](https://github.com/logpai/loghub): Hadoop (HDFS), Blue Gene/L (BGL), and Spark.

### HDFS — 7.5GB decompressed

| Query | xgrep | zgrep | Speedup |
|---|---|---|---|
| Block ID | 30ms | 27s | **913x** |
| `WARN` | 28ms | 25s | **907x** |
| `INFO` (very common) | 23ms | 28s | **1,217x** |

### BGL — 5.0GB decompressed

| Query | xgrep | zgrep | Speedup |
|---|---|---|---|
| Node ID | 26ms | 17s | **655x** |
| `FATAL` | 25ms | 17s | **708x** |

### Spark — 3,852 files

| Query | xgrep | zgrep | Speedup |
|---|---|---|---|
| `Executor` | 5s | 10m | **118x** |
| `ERROR` | 2.7s | 10m | **220x** |

These are repeated-query (cached) results.

First query is still ~18x faster than zgrep (parallel decompression), but the real win is every query after that — which is how incident debugging actually works.

## JSON logs (jq)

```bash
zcat logs.json.gz | jq 'select(.user_id == 42042)'   # 40s
xgrep -j 'user_id=42042' logs.json.gz               # 0.22s
```

- 188x faster
- 97% block skip
- exact results (no misses)

Even broad queries (no skipping) are ~20x faster because xgrep avoids jq's full JSON evaluation.

## How it works (short version)

1. **Index**: decompress once, split into blocks, build bloom filters
2. **Query**: check filters, read only candidate blocks
3. **Execution**: memory-mapped, OS loads only what's needed

The key metric isn't speed. It's **bytes touched per query.**

- zgrep: 100% every time
- xgrep: 0.1-1%

That's why the gap grows with data size.

## Tradeoffs (honest)

- **Cache size**: ~5x compressed size (stores decompressed data)
- **First run**: ~2 min index build (amortized quickly)
- **Not universal grep**: built for compressed logs + repeated search
- For plain text: use ripgrep.

## Who this is for

If you've ever:

- waited on `zgrep` during an incident
- rerun the same search 10 times
- dealt with rotated `.gz` logs
- wanted log-platform speed without log-platform overhead

## Try it

```bash
cargo install xgrep-cli
xgrep "ERROR" logs/*.gz
```

[github.com/HighpassStudio/xgrep](https://github.com/HighpassStudio/xgrep)

## Deep dive

Architecture + benchmark methodology: [ARCHITECTURE.md](ARCHITECTURE.md)

---

*xgrep is Apache-2.0 licensed. Built with Rust, rayon, memchr, and flate2.*
