#!/usr/bin/env python3
"""Relevance gate: replays a golden set against one or more engines.

The golden set is private (it describes a real workspace): $CTXQ_GOLDEN,
otherwise eval/golden.jsonl (ignored by git). Format: eval/README.md.

Scoring: expect/all substrings over source + breadcrumb + text, top K. Engine:
  next   - this repository (target/release/ctxq); index in $CTXQ_CACHE_DIR
           (or $DN_CACHE, default ~/.cache/ctxq), extra flags in $DN_ARGS
           (e.g. "--embeddings ollama"). `--corpus` is resolved from this
           process's cwd; the binary is launched with cwd=/tmp so a
           repo-root ctxq.toml cannot leak into the gate.
Usage: bench/eval_docs.py [--engines next] [--corpus eval/public/corpus] [-v]
"""
import argparse, json, os, random, re, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GOLD = Path(os.environ.get("CTXQ_GOLDEN", ROOT / "eval" / "golden.jsonl"))
BINS = {"next": str(ROOT / "target" / "release" / "ctxq")}
K = 5

def rank(results, g):
    any_n = [e.lower() for e in g.get("expect", [])]; all_n = [e.lower() for e in g.get("all", [])]
    for i, r in enumerate(results, 1):
        hay = f'{r["source"]}\n{r.get("breadcrumb","")}\n{r["text"]}'.lower()
        if any_n and not any(n in hay for n in any_n): continue
        if all_n and not all(n in hay for n in all_n): continue
        return i
    return 0

def binary(name):
    def run(q, corpus):
        env = dict(os.environ)
        extra = []
        if name == "next":
            if os.environ.get("DN_CACHE"):
                env["CTXQ_CACHE_DIR"] = os.environ["DN_CACHE"]
            extra = os.environ.get("DN_ARGS", "").split()
        # cwd=/tmp so a developer ctxq.toml at the repo root cannot leak in.
        # The corpus must still be the tree we indexed, so resolve it first.
        corpus = str(Path(corpus).expanduser().resolve())
        proc = subprocess.run(
            [BINS[name], *extra, "--path", corpus, "search", q, "-l", str(K),
             "--json", "--no-daemon"],
            capture_output=True, text=True, cwd="/tmp", env=env,
        )
        if proc.returncode != 0:
            raise SystemExit(
                f"ctxq search failed (exit {proc.returncode}) path={corpus}: "
                f"{(proc.stderr or proc.stdout).strip()[:800]}"
            )
        try:
            payload = json.loads(proc.stdout or "{}")
        except json.JSONDecodeError:
            raise SystemExit(f"ctxq search returned non-JSON: {proc.stdout[:800]!r}")
        res = [{"source": r.get("source", ""), "breadcrumb": r.get("breadcrumb") or "",
                "text": r.get("text", "")} for r in payload.get("results", [])]
        return res
    return run

ENGINES = {"next": binary("next")}

def metrics(ranks):
    n = len(ranks) or 1
    return {"r@1": sum(0 < r <= 1 for r in ranks) / n, "r@3": sum(0 < r <= 3 for r in ranks) / n,
            "r@5": sum(0 < r <= 5 for r in ranks) / n, "mrr": sum(1 / r for r in ranks if r) / n,
            "miss": sum(r == 0 for r in ranks)}

def ci90(ranks, n=2000):
    rnd = random.Random(0); rr = [1 / r if r else 0 for r in ranks]
    s = sorted(sum(rnd.choice(rr) for _ in rr) / len(rr) for _ in range(n))
    return s[int(n * .05)], s[int(n * .95)]

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engines", default="next")
    ap.add_argument("--corpus", default="/tmp/dn-corpus")
    ap.add_argument("--out", default=os.environ.get("CTXQ_EVAL_OUT", str(ROOT / "bench" / "last_rows.json")))
    ap.add_argument("-v", action="store_true")
    ap.add_argument("--min-mrr", type=float, default=0.0, help="fail if overall MRR is below this")
    a = ap.parse_args()
    names = a.engines.split(",")
    gold = [json.loads(l) for l in open(GOLD) if l.strip() and not l.startswith("//")]
    rows = []
    for g in gold:
        row = {"q": g["q"], "kind": g.get("kind", "?")}
        for name in names:
            t = time.perf_counter(); res = ENGINES[name](g["q"], a.corpus)
            row[name + "_ms"] = (time.perf_counter() - t) * 1000; row[name] = rank(res, g)
        rows.append(row)
        if a.v: print(" ".join(f"{n}={row[n]}" for n in names), g["q"])
    json.dump(rows, open(a.out, "w"), ensure_ascii=False, indent=1)
    for kind in ("all", "exact", "semantic"):
        sub = [r for r in rows if kind == "all" or r["kind"] == kind]
        if not sub: continue
        print(f"\n### {kind} (n={len(sub)})")
        for name in names:
            rk = [r[name] for r in sub]; m = metrics(rk); lo, hi = ci90(rk)
            lat = sorted(r[name + "_ms"] for r in sub)[len(sub) // 2]
            print(f"{name:6} r@1={m['r@1']:.2f} r@3={m['r@3']:.2f} r@5={m['r@5']:.2f} "
                  f"MRR={m['mrr']:.3f} [{lo:.3f}–{hi:.3f}] miss={m['miss']} p50={lat:.0f}ms")
            if kind == "all" and a.min_mrr and m["mrr"] < a.min_mrr:
                print(f"FAIL: MRR {m['mrr']:.3f} < --min-mrr {a.min_mrr}", file=sys.stderr)
                return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
