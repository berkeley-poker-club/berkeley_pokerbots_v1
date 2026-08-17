# TODO / follow-ups

Done in the sprint: engine correctness + tests, bot protocol + reference bots + SDK, local CLI,
SQLite store, artifact store, process/docker sandboxes, job queue + workers, competition API,
nightly scheduler, leaderboard, OpenAPI, docs, docker-compose, CI.

## Next
- [ ] Postgres `Store` implementation (schema is written to be portable; see `store.rs`) for
      multi-host worker fleets; S3-compatible artifact store with local cache.
- [ ] Docker sandbox: exercised only by inspection here (no Docker on the dev machine) — run the
      e2e test with `POKERBOTS_SANDBOX=docker` on the club server before the first nightly.
- [ ] Long-lived bot containers reused across the tournaments of a series (saves ~200 container
      starts per tournament).
- [ ] Per-key request rate limiting at the edge (nginx) — the API only limits on-demand runs.
- [ ] Optional static leaderboard page / web upload UI on top of the API.
- [ ] Berkeley SSO wrapper issuing team API keys.
- [ ] Hand-history download for teams (currently admin-only because logs reveal hole cards).
