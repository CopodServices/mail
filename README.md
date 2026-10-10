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
| GET | /api/messages | List stored mail (filter with ?mailbox=&limit=) |
| GET | /api/messages/{id} | Message metadata |
| GET | /api/messages/{id}/raw | Raw RFC5322 message from S3 |
| DELETE | /api/messages/{id} | Remove the row and the stored object |
| GET | /api/mailboxes | List mailboxes |
| POST | /api/mailboxes | Create a mailbox ({address, password}) |
| DELETE | /api/mailboxes/{address} | Delete a mailbox |
| GET | /api/domains | List domains |
| POST | /api/domains | Create a domain with a fresh DKIM key |
| GET | /api/domains/{name}/dkim | The DKIM TXT record to publish |
| POST | /api/send | Compose and deliver ({from, to, subject, text, html}) |

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
