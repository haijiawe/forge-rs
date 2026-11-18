---
name: 发布小组
slug: release-squad
description: Reviews a release candidate before it ships
coordinator: release-lead
members:
  - expert: rust-concurrency-review
    duty: reviews the lock ordering
  - expert: bench-owner
    duty: re-records the baselines
  - expert: api-reviewer
    duty: checks the public surface for accidental breaks
---
Team-level instructions.

Land nothing until every report is in. The combined verdict is either "ships"
or a list of what is missing; "looks fine" is not a verdict.
