# CoPod agent guide

This checkout is the CoPod **mail** repository. CoPod is four separate git
repositories — `backend`, `dashboard`, `daemon`, `mail` — cloned side by side next to a
fifth `agents` checkout. The consolidated, primary agent guide for all of them lives in
that repository:

    ../agents/AGENTS.md

Read `../agents/AGENTS.md` first, before changing anything here. It is the entry point
for the whole workspace: it covers what each repository is, its layout, build and test
commands, coding conventions, commit and PR rules, and configuration and security notes.

The committed files in this repository — `README.md`, `.github/workflows/`, and
`compose.yml` / `Makefile` / `.env.example` where they exist — hold this repository's own
detail. Where they disagree with the main guide, they win; update the main guide to match.

If `../agents/AGENTS.md` cannot be read (the `agents` checkout is missing), say so
instead of guessing at the workspace conventions.

