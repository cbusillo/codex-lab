# Direction

This file is the current direction for Codex Lab. When an issue, milestone,
or other document disagrees with it, this file wins and the other source is
corrected or closed. Issues are a work list, not instructions.

## Purpose

Codex Lab is parked. What the owner needed from it now comes from elsewhere:

- Remote access to CLI sessions is the Discord bridge in
  `cbusillo/discord-blue`. It runs on stock Codex's app-server, and Claude
  Code sessions are joining it.
- Separate execution accounts are dropped. The account router is retired.
- Patches to upstream's files would mean running an own build, which is
  retired below, so they are not a fallback.

Stock Codex, the shared catalog, and sidecars in their own repositories cover
the current needs. This repository stays as a reference until the owner
decides whether to archive the fork, as with the old Lab main, or give it a
new purpose.

## Stop Boundaries

An agent asks the owner before:

- starting work in this repository beyond keeping its records accurate
- replacing or rewriting the default branch
- starting a catch-up with upstream
- changing credentials, account storage, or login flows
- publishing a release
- writing to openai/codex or any other person's repository

Everything else is ordinary engineering and needs no ceremony.

## Journey

None while parked. The owner's remote journey continues in
`cbusillo/discord-blue`: the owner starts a CLI session, Codex or Claude
Code, and gets and gives quick updates from Discord when away from the
computer.

## Retired

- the old Lab main and its routine catch-ups; it is archived, never
  deleted, and code comes back from it only for a kept need
- automatic reviews, validation, and command policies inside the engine;
  they live in catalog hooks
- discord-blue and the remote inbox as requirements of this repository;
  the Discord bridge now lives in `cbusillo/discord-blue`
- Auto Drive, and Every Code (`code`) as a runtime
- the installed Lab build and its services: the remote-control app-server,
  housekeeping jobs, the self-hosted release and signing runners, and the
  Every Code worker. Nothing depends on them; their sessions are kept as
  evidence before removal
- the account router and switching execution accounts between turns
  (2026-09-28): the owner won't use it, and every stock release needed a
  new qualification
- the milestones `Remote access on the fresh start` and `Other needs on
  the fresh start`: remote access moved to `cbusillo/discord-blue`, and the
  remaining account and agent needs are parked with this repository

A retired concept comes back only through a direction change, in a shape
that fits this file.

## Milestones

None while parked.
