# meilie_updater

`meilie_updater` is an HTTP-triggered background job service. It is **not** a
CLI tool: it runs as a long-lived web server (built with `axum`) that, when
triggered, reads books/authors/sequences/genres from PostgreSQL and reindexes
them into Meilisearch.

The server listens on port `8080`.

## Endpoints

### `POST /update`

Triggers a reindex run. Requires an `Authorization` header containing the
API key, compared against the `API_KEY` env var using a constant-time
comparison.

Responses:

- `401 Unauthorized` — the `Authorization` header is missing, not valid
  UTF-8, or doesn't match `API_KEY`.
- `202 Accepted` — the request was accepted; the reindex run has been
  spawned as a background task and the response returns immediately
  without waiting for it to finish. If already running, requests coalesce into
  one pending rerun. If another replica holds the PostgreSQL advisory lock,
  the worker retries once per second until it can run.

### `GET /status`

Requires the `Authorization` header (same as `POST /update`). Returns
`{"running": bool, "last_run": ...}` where `last_run` holds per-index document
counts, start time, duration, `outcome` (`success`, `failed`, `timeout`, or
`busy`) and `error` (`null` until a run has completed). Errors and timeouts
replace the previous result, rather than leaving an old success visible.

Each run holds a dedicated PostgreSQL session advisory lock, builds all four
indexes sequentially from one read-only REPEATABLE READ transaction, validates
them, and publishes with a single atomic multi-index swap. Staging UIDs include
a run ID; only the known `books/authors/sequences/genres_new` and `_new_*`
names are cleaned while holding the lock. All replicas sharing these Meili
indexes must use the same PostgreSQL database and this locking protocol; stop
old-version workers before upgrading. Failures before publication swap nothing.
A timeout/network failure while waiting for the swap may have an unknown
publication outcome, but the swap itself still publishes all four or none.
Own staging indexes are cleaned on success/failure/timeout; crash leftovers are
removed on the next run. Cleanup is best effort and may extend the run budget.

### `GET /health`

Simple liveness check (`200 OK`). Used by the Docker image's
`HEALTHCHECK`.

## Configuration

All configuration is provided via environment variables (see
`src/config.rs`); the process panics on startup if any are missing:

| Variable             | Purpose                                   |
|----------------------|--------------------------------------------|
| `API_KEY`            | Shared secret required on `POST /update`  |
| `SENTRY_DSN`         | Sentry DSN for error reporting            |
| `POSTGRES_DB_NAME`   | PostgreSQL database name                  |
| `POSTGRES_HOST`      | PostgreSQL host                           |
| `POSTGRES_PORT`      | PostgreSQL port                           |
| `POSTGRES_USER`      | PostgreSQL user                           |
| `POSTGRES_PASSWORD`  | PostgreSQL password                       |
| `MEILI_HOST`         | Meilisearch host URL                      |
| `MEILI_MASTER_KEY`   | Meilisearch master key                    |

The following are optional and tunable, with sane defaults — the process
does **not** panic on startup if they're missing:

| Variable                    | Purpose                                                      | Default   |
|-----------------------------|---------------------------------------------------------------|-----------|
| `STATEMENT_TIMEOUT_MS`      | Postgres session default; snapshot streaming overrides it to the run budget | `300000` |
| `UPDATE_TIMEOUT_SECS`      | Build/publish budget (cleanup can extend this) | `10800` |
| `POOL_MAX_SIZE`             | Maximum number of connections in the Postgres pool             | `8`       |
| `POOL_WAIT_TIMEOUT_SECS`    | Max time to wait for a free pool connection                    | `5`       |
| `POOL_CREATE_TIMEOUT_SECS`  | Max time to wait when creating a new pool connection            | `5`       |
| `POOL_RECYCLE_TIMEOUT_SECS` | Max time to wait when recycling a pool connection               | `5`       |
| `BATCH_SIZE`                | Rows streamed from Postgres per Meilisearch `add_or_update` batch | `1024`  |

### Tuning `BATCH_SIZE` and peak memory

Each batch of rows is fully materialized twice while in flight: once as the
collected `Vec<T>` and once as the Meilisearch SDK's serialized JSON request
body. With models updating sequentially,
peak memory is roughly:

```
peak ≈ BATCH_SIZE × avg_document_size_bytes × 2
```

Lowering `BATCH_SIZE` trades indexing throughput (more round-trips to
Meilisearch) for lower peak RAM — useful when running with a constrained
memory limit or when documents are unusually large (e.g. books with many
authors/genres).

## Running

The service listens on `0.0.0.0:8080`. See `docker/build.dockerfile` for the
container build (non-root runtime user, `HEALTHCHECK` against
`GET /health`).
