# Search and ranking

ctxq ranks with a global BM25 plus dense cosine, fused as
`cosine + α · bm25 / bm25_max`. Identifier-like queries (a port, a container
name, `name.ext`) get a higher lexical weight than natural-language questions.

The built-in embedder is `local-hash-v1-128`: hashed n-grams, 128 dimensions,
fully offline. It is not a neural model. With that embedder, BM25 leads and
the hash is a weak rerank. Neural semantic search needs `[embed] backend = "ollama"`
or `"openai"` in `ctxq.toml`.

The first `search` starts a daemon that keeps the index warm. Default refresh:
docs every 60 seconds, inventories every 300 seconds.
