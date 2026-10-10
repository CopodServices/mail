# mail

Inbound mail server for CopodServices. It accepts SMTP for the domains it owns,
verifies the message, stores it in S3 and indexes it in PostgreSQL. Every
outbound message is DKIM-signed and handed to a smarthost relay; this service
never delivers directly to a recipient's MX.

## What it does

- **Inbound (:2525 locally, :25 in production)** — EHLO advertises SIZE, 8BITMIME,
  PIPELINING, ENHANCEDSTATUSCODES and STARTTLS when a certificate is configured.
  Each RCPT is validated against a local mailbox; unknown recipients are refused
  with 550, so the listener can never be used as an open relay. On DATA the
  message is parsed (mail-parser), authenticated (SPF/DKIM/DMARC via mail-auth),
  written to S3 and indexed in PostgreSQL. A DMARC failure with a rejecting
  policy is refused with 550; a quarantining policy stores the message with
  status quarantined.
- **Submission (:2587 locally, :587 in production)** — requires AUTH (PLAIN or
  LOGIN, argon2 password hashes) and the envelope sender must match the
  authenticated mailbox. Mail to a local mailbox is stored; anything else is
  queued for the relay.
- **Relay worker** — claims queued messages, adds a DKIM signature with the
  sender domain's key and delivers to the smarthost over STARTTLS or implicit
  TLS with AUTH, retrying with exponential backoff (capped at six hours).

## Components

The SMTP server loop, spooling and delivery are ours; the protocol work comes
from Stalwart's libraries. Stalwart's mail server itself is not embeddable, so
there is no server library to reuse.

| Crate | Used for |
| --- | --- |
| smtp-proto | parsing SMTP commands and building replies |
| mail-parser | MIME parsing, subject/sender/message-id metadata |
| mail-auth | SPF, DKIM, DMARC verification and DKIM signing |
| mail-builder | composing API-submitted messages |

## Local development

Requires Docker, and a Rust toolchain (edition 2024).

    cp .env.example .env
    make up            # Postgres, an S3 mock (bucket created), Mailpit
    make migrate
    make dev           # inbound :2525, submission :2587, HTTP API :8080

The stack is fully offline: Postgres, an S3-compatible mock and Mailpit. Mailpit
(http://localhost:8025) is the relay target, so anything the service relays shows
up there. API docs are at http://localhost:8080/scalar.

Smoke test the inbound path (the .env seeds postmaster@example.test):

    curl -sS -X POST http://localhost:8080/api/send \
      -H 'content-type: application/json' \
      -d '{"from":"postmaster@example.test","to":["postmaster@example.test"],
           "subject":"hello","text":"first message"}'

    curl -sS http://localhost:8080/api/messages | head -c 400

## HTTP API

All routes require Authorization: Bearer <API_KEY> unless API_KEY is empty.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | /api/messages | List stored mail. Filters: mailbox, mailbox_id, folder, direction, q, read, starred, limit, offset |
| GET | /api/messages/counts | Per-folder and per-mailbox totals and unread counts |
| GET | /api/messages/{id} | Message with body (text/html) and the attachment list |
| GET | /api/messages/{id}/raw | Raw RFC5322 message from S3 |
| GET | /api/messages/{id}/attachments/{index} | One attachment by position |
| POST | /api/messages/{id}/read | Set read state ({read}) |
| POST | /api/messages/{id}/star | Set starred state ({starred}) |
| POST | /api/messages/{id}/snooze | Snooze until a time ({snoozed_until}) |
| GET | /api/messages/{id}/metadata | Attachments and the unsubscribe link |
| POST | /api/messages/{id}/cancel-schedule | Cancel a scheduled send (returns it to Drafts) |
| DELETE | /api/messages/{id}/snooze | Return it to the inbox now |
| GET | /api/messages/navigation | Ordered ids and unread flags for the open filter |
| GET | /api/messages/{id}/thread | The conversation, with bodies and attachments |
| POST | /api/messages/empty | Empty trash or spam ({mailbox_id, folder}) |
| POST | /api/messages/bulk | Act on many messages ({message_ids, action, folder}) |
| DELETE | /api/messages/{id} | Remove the row and the stored object |
| GET | /api/folders | System and custom folders for a mailbox |
| POST | /api/folders | Create a custom folder ({mailbox_id, name, color}) |
| GET | /api/mailboxes | List mailboxes |
| POST | /api/mailboxes | Create a mailbox ({address, password}) |
| DELETE | /api/mailboxes/{address} | Delete a mailbox |
| PATCH | /api/mailboxes/{address} | Set the mailbox signature ({signature}) |
| GET | /api/domains | List domains |
| POST | /api/domains | Create a domain with a fresh DKIM key |
| GET | /api/domains/{name}/dkim | The DKIM TXT record to publish |
| POST | /api/domains/{name}/dkim | The same DKIM TXT record (GET answers too) |
| GET | /api/attachment-policy | Attachment limits for the composer |
| GET/POST | /api/templates, GET/DELETE /api/templates/{id} | Composer templates |
| GET/PATCH | /api/settings/shortcuts, /api/settings/recipient-addresses | Client preferences |
| GET | /api/contacts | A contact for an address |
| PATCH | /api/contacts | Rename a contact |
| POST | /api/contacts/block | Block a sender |
| GET/POST/DELETE | /api/contacts/avatar | Contact picture, full or preview |
| GET | /api/realtime | WebSocket pushing a frame per stored message |
| POST | /api/drafts | Create a draft ({mailbox_id, from, to, cc, bcc, subject, text, html, in_reply_to, references_header, thread_id, forward_of_message_id}) |
| GET/PATCH/DELETE | /api/drafts/{id} | Read, update or discard a draft |
| POST | /api/drafts/{id}/attachments | Upload attachments (multipart) |
| DELETE | /api/drafts/{id}/attachments/{index} | Remove an attachment |
| POST | /api/send | Compose and deliver ({from, to, cc, bcc, subject, text, html, draft_id, scheduled_at}) |

Reader state (read, starred, folder) lives in its own table, message_flags, so
the MTA's stored messages stay immutable.

## Configuration

| Variable | Purpose |
| --- | --- |
| DATABASE_URL | PostgreSQL connection for the mail database |
| ENCRYPTION_KEY | AES-256-GCM key (base64, 32 bytes) for stored DKIM keys |
| API_KEY | Bearer token for the HTTP API (empty disables auth) |
| SMTP_LISTEN / SUBMISSION_LISTEN | Listener addresses (0.0.0.0:2525 / :2587) |
| MAIL_HOSTNAME | EHLO name of this server |
| MAIL_DOMAINS | Domains to accept mail for (DKIM keys are generated) |
| MAIL_BOOTSTRAP_PASSWORD | Optional password for the seeded postmaster mailbox |
| MAX_MESSAGE_SIZE / MAX_RECIPIENTS | Limits (25 MiB / 50) |
| MAIL_VERIFY_AUTH | Enable SPF/DKIM/DMARC verification on inbound |
| S3_* | Endpoint, region, bucket and credentials (path style works for the mock, R2 and S3) |
| RELAY_* | Smarthost host, port, credentials, TLS mode and optional sender rewrite |
| TLS_CERT_PATH / TLS_KEY_PATH | PEM pair enabling inbound STARTTLS |

## Layout

| Crate | Contents |
| --- | --- |
| mailcore | Config, S3 storage, DKIM, auth policy, relay client, queue worker, persistence |
| entity | SeaORM entities |
| migration + migrator | Schema migrations and the one-shot runner |
| server | The mail binary: SMTP listeners, HTTP API, bootstrap |

## Deployment notes (out of scope for now)

Receiving public mail needs a real TCP port 25 on a public address with matching
PTR, which the Cloudflare Tunnel used by the other services cannot carry. When we
deploy, the split is: forward port 25 to the cluster (or run on a host with its
own public address), publish an MX record, PTR/rDNS for the sending hostname,
the SPF record for the relay, the DKIM TXT record from
GET /api/domains/{name}/dkim, and a DMARC policy. Certificates for STARTTLS are
supplied through TLS_CERT_PATH/TLS_KEY_PATH.

## Web client

web/ is the cPOD Mail web client: a static, client-rendered mail reader for this
API (inbox and threads, compose and send, sent, drafts, spam, trash, starred,
snoozed, archived, folders, search). It is built with bun, formatted with
Biome, and shipped as a static bundle served by nginx, which also proxies /api/*
to this service. It calls the API's own /api/* endpoints; there is no session
yet, so the browser bundle holds no credentials of its own, and authentication
will move to external SSO.

    make web-install       # or: cd web && bun install
    make web-dev           # http://localhost:3002
    make web-build         # static export into web/out
    make web-test          # bun test (set MAIL_API_URL for the contract suite)
    docker build -t copod-mail-web web
    make web-up            # docker compose up -d --build web (http://localhost:3002)

See web/README.md, docs/mail-api.md, and NOTICE for the contract and the
Mailflare attribution.
