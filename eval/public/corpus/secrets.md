# Secrets

Every file is scanned before chunking and embedding. A file that holds a key,
token or private PEM is quarantined: never chunked, never embedded, never
served. `status` lists quarantined paths.

The API key for a remote embedder never goes in `ctxq.toml`. Set
`api_key_env = "OPENAI_API_KEY"` and keep the value in the environment.
