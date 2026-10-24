# Golden sets

`bench/eval_docs.py` replays questions against an index and measures recall@k
and MRR (bootstrap confidence interval).

## Public gate (in git, CI)

`eval/public/` is a tiny self-contained corpus + questions about ctxq itself.
It is what a contributor can run:

```bash
cargo build --release
./target/release/ctxq --path eval/public/corpus index
CTXQ_GOLDEN=eval/public/golden.jsonl python3 bench/eval_docs.py \
  --corpus eval/public/corpus --min-mrr 0.80
```

## Private set

A 100-question set from a real private workspace stays out of the repository
(`eval/golden.jsonl` is gitignored, or `$CTXQ_GOLDEN`). That is the number
quoted on the site (MRR 0.795 with qwen3-embedding).

One question per line (JSONL, `//` lines are ignored):

```json
{"q": "which port does the API run on?", "expect": ["8080"], "kind": "exact", "split": "test"}
{"q": "how to deploy the front end", "all": ["deploy", "front"], "kind": "semantic", "split": "dev"}
```

- `expect`: any one of the substrings is enough; `all`: all of them in the same
  passage (source + breadcrumb + text).
- `kind`: `exact` (identifier) or `semantic`.
- `split`: `dev` for tuning, `test` for the final measurement.
