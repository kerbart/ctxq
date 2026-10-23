# Golden set (private)

`bench/eval_docs.py` replays a set of questions against the index and measures
recall@k and MRR (bootstrap confidence interval). The set describes a real
workspace, so it stays out of the repository (`eval/golden.jsonl` is ignored by
git, or use `$DOCUMENTUM_GOLDEN`).

One question per line (JSONL, `//` lines are ignored):

```json
{"q": "which port does the API run on?", "expect": ["8080"], "kind": "exact", "split": "test"}
{"q": "how to deploy the front end", "all": ["deploy", "front"], "kind": "semantic", "split": "dev"}
```

- `expect`: any one of the substrings is enough; `all`: all of them in the same
  passage (source + breadcrumb + text).
- `kind`: `exact` (identifier) or `semantic`.
- `split`: `dev` for tuning, `test` for the final measurement.
