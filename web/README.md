# cPOD Mail web client

A mail web client (a Roundcube-style reader) for the cPOD Mail API: sign-in-free
for now, inbox and threads, compose and send, sent, drafts, spam, trash, starred,
snoozed, archived, custom folders, and search.

It is a static, client-rendered bundle (no server rendering, no Node runtime in
production). Every page loads its data in the browser from the mail API.

## Stack

- Next.js 16 App Router, exported statically (output: "export")
- React 19, Tailwind 4, Radix primitives
- bun for install, scripts and the container build
- Biome for lint and formatting (bun run lint / bun run format)
- nginx serving the export and proxying /api/* in the container

## Running

    bun install
    bun run dev        # http://localhost:3002

The dev server talks to the API at the same origin. To point it elsewhere, set
NEXT_PUBLIC_MAIL_API_URL (for example http://localhost:8080) and
NEXT_PUBLIC_MAIL_API_KEY in .env, then restart.

## API

The client calls the mail API directly under /api/*. There is no session yet:
requests carry an API key as "Authorization: Bearer <key>" when one is set
(NEXT_PUBLIC_MAIL_API_KEY, or a value saved in the browser), and authentication
will move to external SSO later. See ../docs/mail-api.md for the endpoints the
client uses.

The API base URL is a run-time decision, not a build-time one. The bundle is
built with NEXT_PUBLIC_MAIL_API_URL empty, so the browser only ever calls this
origin under /api/*; in the container nginx forwards those requests to the host
in MAIL_API_URL, read from the environment when the container starts. Pointing
the client at a different API is then a container restart, not a rebuild, and
the client needs no CORS setup.

An API key is the one thing that must reach the browser, so it is either baked
in with NEXT_PUBLIC_MAIL_API_KEY or saved in the browser (localStorage key
"copod:api-key"); a saved key wins.

## Routes

Inbox, starred, snoozed, sent, scheduled, drafts, archived, spam, trash, custom
folders, compose, and settings (appearance, reading layout, threading,
shortcuts and notifications — all stored locally).

Message selection travels in the query string (/inbox?message=<id>,
/folders?folder=<id>&message=<id>) because a static export has no server routing.

## Tests

bun run test runs the client's own API helpers and adapters (tests/), but the
contract suite needs a live API:

    MAIL_API_URL=http://127.0.0.1:8080 bun test

Without MAIL_API_URL the suite is skipped, so lint and build need no server.

## Build and container

    bun run build      # emits ./out
    docker build -t copod-mail-web .
    docker run -p 8080:8080 -e MAIL_API_URL=https://mail.example copod-mail-web

MAIL_API_URL is read when the container starts and must be a bare origin that
resolves from inside the container (no trailing slash): nginx resolves it once,
at start-up, and forwards the /api prefix unchanged.

## Attribution

The UI is a reimplementation of the Mailflare mail client by Hieu Nguyen
(https://github.com/hieunc229/mailflare, AGPL-3.0). The upstream project is
credited in ../NOTICE and the repository README. This project is licensed
AGPL-3.0; the sidebar footer links to the source.
