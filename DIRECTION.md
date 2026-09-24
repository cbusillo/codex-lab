# Direction

This file is the current direction for Codex Lab. When an issue, milestone,
or other document disagrees with it, this file wins and the other source is
corrected or closed. Issues are a work list, not instructions.

## Purpose

Codex Lab gives the owner what stock Codex plus the shared catalog cannot:
good-enough remote access (getting and giving quick updates to CLI sessions
when away from the computer, by any route that works, including the ChatGPT
app) that integrates with Launchplane for GitHub and itself; many accounts
with separate GUI and TUI logins; and agents from other providers that
behave like Codex agents. The repository holds upstream Codex, sidecars that
run beside it, and the fewest patches the needs require. Skills and hooks
live in the shared catalog.

Judge every change by one question: does this have to be inside Codex? If a
hook, skill, or sidecar on stock Codex could do it, it does not belong in
upstream's files. Every line kept inside upstream's files has to justify
its catch-up cost.

## Stop Boundaries

An agent asks the owner before:

- replacing or rewriting the default branch
- starting a catch-up with upstream
- changing credentials, account storage, or login flows
- publishing a release
- writing to openai/codex or any other person's repository

Everything else is ordinary engineering and needs no ceremony.

## Journey

On the fresh start: the owner starts a CLI session, gets and gives a quick
update from away, switches accounts when one hits its limit, and hands a
task to a Claude agent that reports back like a Codex agent. Each step works
through stock Codex, the catalog, a sidecar, or one small patch. Whatever
step does not work yet is the next piece of work.

## Retired

- the old Lab main and its routine catch-ups; it is archived, never
  deleted, and code comes back from it only for a kept need
- automatic reviews, validation, and command policies inside the engine;
  they live in catalog hooks
- discord-blue and the remote inbox as requirements; remote access that
  works is the requirement
- Auto Drive, and Every Code (`code`) as a runtime
- the installed Lab build and its services: the remote-control app-server,
  housekeeping jobs, the self-hosted release and signing runners, and the
  Every Code worker. Nothing depends on them; their sessions are kept as
  evidence before removal

A retired concept comes back only through a direction change, in a shape
that fits this file.

## Milestones

- `Fresh start on upstream` proves main is upstream HEAD, the old main is
  archived, and the retired installed build and its services are removed
  after their sessions are saved; ends if the archive is lost.
- `Remote access on the fresh start` proves the owner gets and gives quick
  updates to a CLI session from away, with Launchplane integration; ends if
  it needs broad patches to upstream's files.
- `Other needs on the fresh start` proves multiple accounts and other
  providers' agents work through sidecars or small patches, with the number
  of upstream files changed recorded; ends if any need costs more than a day
  per catch-up.
