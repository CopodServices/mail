# cPOD Mail API and the web client

The web client (web/) is a static, client-rendered bundle. It talks to this
service directly under /api/* and, in the container, nginx proxies /api/* to
MAIL_API_URL (read from the environment at container start), so no CORS setup is
needed and the API address does not have to be baked into the bundle.

## Authentication

There are two kinds of caller, and the API tells them apart on every route:

- **Operator** — Authorization: Bearer API_KEY. This is the admin credential:
  it manages every tenant and is what domains and mailboxes are created with.
  When API_KEY is unset (development) an unauthenticated caller is the operator.
- **Mailbox session** — the cookie the web client gets from POST /api/session.
  It reaches exactly one mailbox: the client's mailbox filters are ignored, so
  every list, read, write, draft, contact, template, preference and realtime
  frame is scoped to it, and anything else answers 404.

| Method | Path | Purpose |
| --- | --- | --- |
| POST | /api/session | {address, password} -> verify the mailbox password and set the cookies |
| GET | /api/session | The signed-in mailbox (null for the operator key) |
| DELETE | /api/session | Clear the session and CSRF cookies |

The session cookie (cpod_mail_session) is httpOnly, SameSite=Lax and Secure when
SESSION_COOKIE_SECURE is set; it carries a 12-hour JWT. The CSRF cookie
(cpod_mail_csrf) is readable so the client can send its value back in the
X-CSRF-Token header on every non-GET request — a cookie-authenticated write
without it is refused with 403. The client sends everything through one wrapper
(web/src/lib/api/client.ts): credentials: "same-origin", the CSRF header on
writes, and a return to /login on any 401. Cross-origin access is deliberately
not supported: the client is served same-origin, with nginx proxying /api/*.

## Operator endpoints (API key)

Tenant lifecycle, behind the API key:

| Method | Path | Purpose |
| --- | --- | --- |
| GET | /api/domains | List domains |
| POST | /api/domains | Create a domain (idempotent) |
| GET | /api/domains/{name} | One domain |
| PATCH | /api/domains/{name} | Set {active}; a disabled tenant stops sending and receiving |
| DELETE | /api/domains/{name} | Remove a domain and its mailboxes |
| GET | /api/mailboxes | List mailboxes with `used_bytes` and `quota_bytes` (?domain=<name> filters) |
| POST | /api/mailboxes | Create a mailbox ({address, password}) |
| PATCH | /api/mailboxes/{address} | {signature, display_name, forward_to, password, quota_bytes, active} |
| DELETE | /api/mailboxes/{address} | Delete a mailbox |

Every mailbox reports its storage as `used_bytes` (the sum of its stored message
sizes) and `quota_bytes` (0 for unlimited) on `GET /api/mailboxes` and the
create/update responses; the webmail footer shows the meter. An inbound message
that would push a mailbox past its quota is refused with a temporary 451 and
stores nothing, so the sender retries later.

DKIM is not part of this API: the configured relay signs outbound mail, so each
tenant domain publishes the authentication records that provider asks for
(typically SPF, a return-path CNAME, a DKIM CNAME and DMARC). See the Relay
section of ../README.md; the provider itself is only RELAY_* configuration.


## Endpoints

The client's whole reading path is served by the API today:

| Method | Path | Purpose |
| --- | --- | --- |
| GET | /api/messages | List mail. Filters: mailbox, mailbox_id, folder, direction, q, read, starred, limit, cursor |
| GET | /api/messages/counts | Folder and mailbox totals plus unread counts, including custom folders |
| GET | /api/messages/{id} | Message with body (text/html) and attachment list |
| GET | /api/messages/{id}/raw | Raw RFC5322 message |
| GET | /api/messages/{id}/attachments/{index} | Attachment bytes by position; ?preview=1 serves a renderable part inline |
| POST | /api/messages/{id}/read | Set read state |
| POST | /api/messages/{id}/star | Set starred state |
| POST / DELETE | /api/messages/{id}/snooze | Snooze until a time, or return it now |
| GET | /api/messages/{id}/metadata | Attachments and the unsubscribe link |
| POST | /api/messages/{id}/cancel-schedule | Cancel a scheduled send; it returns to Drafts |
| GET | /api/messages/navigation | Ordered ids and unread flags for the open filter |
| GET | /api/messages/{id}/thread | The conversation, reassembled from In-Reply-To and References |
| POST | /api/messages/bulk | archive, trash, spam, inbox, read, unread, starred, unstarred, folder, delete |
| DELETE | /api/messages/{id} | Delete a message |
| GET | /api/folders | System and custom folders |
| POST | /api/folders | Create a custom folder |
| GET | /api/mailboxes | Mailboxes (a session sees only its own) |
| PATCH | /api/mailboxes/{address} | Set the mailbox signature, display name or auto-forward address (a session may set only its own) |
| GET | /api/attachment-policy | Attachment limits for the composer |
| POST | /api/drafts, /api/drafts/{id} | Drafts: create, read, update, discard (forward_of_message_id copies its attachments) |
| POST | /api/drafts/{id}/attachments, DELETE /api/drafts/{id}/attachments/{index} | Draft attachments |
| POST | /api/send | Compose and deliver, including cc/bcc and a draft id whose attachments ride along |
| GET/POST | /api/templates, GET/DELETE /api/templates/{id} | Composer templates |
| GET/PATCH | /api/settings/shortcuts, /api/settings/recipient-addresses | Client preferences |
| GET | /api/settings/clients | Submission host, port, TLS mode and username for another mail app |
| GET/PATCH | /api/contacts | Read or rename a contact |
| POST | /api/contacts/block | Block a sender |
| GET/POST/DELETE | /api/contacts/avatar | Contact picture, full or preview |
| GET | /api/realtime | WebSocket that pushes a frame per stored message |

Reader state is stored in message_flags (read, starred, folder, snoozed_until);
a snoozed message appears in the Snoozed folder until its time passes, then
returns to its folder on its own. The message row
itself is never mutated. Threading uses the In-Reply-To and References headers,
captured at intake into the message row.

Only mail that arrived can be unread: the copy kept in the sender's own Sent
folder reports its delivery state instead (send_state and send_error, from the
queued job that carries it, and scheduled_at while it waits). A message without
a reader flag is classified by its direction and delivery status, and the list,
the counts and the reading order all classify that way in SQL, so a folder's
badge always matches the rows it opens.

Client preferences are one settings row per mailbox (keyed by the mailbox
address), and templates are a small table the composer lists and fills from.

Attachment bytes are served by position within the parsed message. With
?preview=1 a part the browser can render in place (an image, a PDF, audio,
video, plain text, CSV or JSON) is served inline; every other type, including
HTML, SVG and XML, stays a download so a preview URL can never run script in the
mail origin. Without the query the part always downloads.

A mailbox may carry one auto-forward address (forward_to). A copy of each
message delivered to that mailbox is queued to the address. The relay refuses a
message whose From domain it has not verified, so the copy goes out as the
mailbox itself: the mailbox address becomes the From, the original sender moves
to Reply-To, and the Received chain and the signatures taken over the old sender
are dropped, because they no longer describe the message. The body is copied
through unchanged. The copy also carries an X-Copod-Auto-Forwarded header, so a
forward that loops back is delivered but never forwarded again, and forwarding
to the mailbox's own address is ignored.

A draft is an ordinary message row whose reader flag puts it in the drafts
folder, with the composed message in object storage, so the drafts list, counts
and search work without a second store.

Every endpoint the web client calls is now implemented.

A message sent with scheduled_at waits in the Scheduled folder and goes out
when its time arrives: it is queued with that time as its next attempt, and the
worker moves the copy to Sent once the relay accepts it. On the same origin the
container proxies the WebSocket upgrade through.

The message list is keyset-paged, newest first: pass the cursor a page returned
in meta.next_cursor to read on, and the page after it starts strictly after the
last row it saw, so a message arriving mid-walk cannot repeat or skip a row.

## Response shapes

Single objects are wrapped as {"data": ...}; collections add
{"meta": {"total", "limit", "next_cursor"}}. Fields are snake_case. The client
translates these into its own model in web/src/lib/api/adapters.ts and
sends its writes through web/src/lib/api/client.ts, so the API stays the
single source of truth for shapes.
