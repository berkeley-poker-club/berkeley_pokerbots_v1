# TODO / follow-ups

Done in the sprint: engine correctness + tests, bot protocol + reference bots + SDK, local CLI,
SQLite store, artifact store, process/docker sandboxes, job queue + workers, competition API,
nightly scheduler, leaderboard, OpenAPI, docs, docker-compose, CI.
Added since: 500-bot scale (fd limits, weighted jobs, per-worker bot capacity), the
build→smoke→trial validation pipeline with auto-activate, the web UI at `/`, and the autoscaler
(processes/command backends, deadline-driven planning, /admin/autoscale).

## Next
- [ ] Postgres `Store` implementation (schema is written to be portable; see `store.rs`) for
      multi-host worker fleets; S3-compatible artifact store with local cache.
- [x] Docker sandbox — verified live via colima (validation pipeline + tournament in containers,
      isolation flags, cleanup). Re-verify once on the x86-64 club server before the first nightly.
- [ ] Long-lived bot containers reused across the tournaments of a series (saves ~200 container
      starts per tournament).
- [ ] Per-key request rate limiting at the edge (nginx) — the API only limits on-demand runs.
- [ ] Autoscaler `command` backend: template/invocation covered by a unit test, but it has not
      driven a real docker compose / cloud target end to end; `processes` backend is verified live.
- [ ] Berkeley SSO wrapper issuing team API keys.
- [ ] Hand-history download for teams (currently admin-only because logs reveal hole cards).
