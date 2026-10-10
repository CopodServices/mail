# Mail API and the web client

The web client (web/) is a static, client-rendered bundle. It talks to this
service directly under /api/* and, in the container, nginx proxies /api/* to
MAIL_API_URL (read from the environment at container start), so no CORS setup is
needed and the API address does not have to be baked into the bundle.

## Authentication

The web client is a plain browser client and holds no credentials: it never
sends an API key or any other secret. API keys belong to the server-side
services that call this API, and a deployment that requires one for the browser
has to front the API with a service that injects it. Browser authentication
will move to external SSO; the client routes every request through one place so
that stays a single change.

## Domain endpoints (API key)

The three endpoints the API exposes for domains stay behind the API key:

| Method | Path | Purpose |
| --- | --- | --- |
| GET | /api/domains | List domains |
| POST | /api/domains | Create a domain with a fresh DKIM key (idempotent) |
| POST | /api/domains/{name}/dkim | The DKIM TXT record to publish (GET also answers) |


## Endpoints

The client's whole reading path is served by the API today:

| Method | Path | Purpose |
| --- | --- | --- |
| GET | /api/messages | List mail. Filters: mailbox, mailbox_id, folder, direction, q, read, starred, limit, offset |
| GET | /api/messages/counts | Folder and mailbox totals plus unread counts, including custom folders |
| GET | /api/messages/{id} | Message with body (text/html) and attachment list |
| GET | /api/messages/{id}/raw | Raw RFC5322 message |
| GET | /api/messages/{id}/attachments/{index} | Attachment bytes by position |
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
| GET | /api/mailboxes | Mailboxes |
| PATCH | /api/mailboxes/{address} | Set the mailbox signature |
| GET | /api/attachment-policy | Attachment limits for the composer |
| POST | /api/drafts, /api/drafts/{id} | Drafts: create, read, update, discard (forward_of_message_id copies its attachments) |
| POST | /api/drafts/{id}/attachments, DELETE /api/drafts/{id}/attachments/{index} | Draft attachments |
| POST | /api/send | Compose and deliver, including cc/bcc and a draft id whose attachments ride along |
| GET/POST | /api/templates, GET/DELETE /api/templates/{id} | Composer templates |
| GET/PATCH | /api/settings/shortcuts, /api/settings/recipient-addresses | Client preferences |
| GET/PATCH | /api/contacts | Read or rename a contact |
| POST | /api/contacts/block | Block a sender |
| GET/POST/DELETE | /api/contacts/avatar | Contact picture, full or preview |
| GET | /api/realtime | WebSocket that pushes a frame per stored message |

Reader state is stored in message_flags (read, starred, folder, snoozed_until);
a snoozed message appears in the Snoozed folder until its time passes, then
returns to its folder on its own. The message row
itself is never mutated. Threading uses the In-Reply-To and References headers,
captured at intake into the message row.

Client preferences live in a single settings row (there are no accounts yet), and
templates are a small table the composer lists and fills from.

A draft is an ordinary message row whose reader flag puts it in the drafts
folder, with the composed message in object storage, so the drafts list, counts
and search work without a second store.

Every endpoint the web client calls is now implemented.

A message sent with scheduled_at waits in the Scheduled folder and goes out
when its time arrives: it is queued with that time as its next attempt, and the
worker moves the copy to Sent once the relay accepts it. On the same origin the
container proxies the WebSocket upgrade through.

## Response shapes

Single objects are wrapped as {"data": ...}; collections add
{"meta": {"total", "limit", "offset"}}. Fields are snake_case. The client
translates these into its own model in web/src/lib/mail-api/adapters.ts and
sends its writes through web/src/lib/mail-api/client.ts, so the API stays the
single source of truth for shapes.
