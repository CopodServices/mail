# Antispam

An in-house spam engine for the cPOD Mail service, modeled on Stalwart's
spam-filter design but re-implemented. It scores an inbound message from a set
of analyzers and, past a threshold, files it in the spam folder. Nothing is
rejected by default.

## Turning it on

The engine is inert until `SPAM_MODE` is not `off`:

| Mode | Behavior |
| --- | --- |
| `off` | no scanning at all (the service default) |
| `log` | score and record the verdict, but always deliver to the inbox |
| `quarantine` | file over-threshold mail in the spam folder |

The k3s deployment in the infrastructure repo ships with `SPAM_MODE=quarantine`;
set it to `log` first if you want to watch the verdicts before filing.
`SPAM_ALLOW_REJECT=true` lets the `reject`/`discard` actions act; left off, every
junk message is quarantined.

## How a message is scored

Analyzers each add a named tag; a tag -> score map (first glob match wins)
turns the tags into numbers; the total is compared against the thresholds.

    ip -> dmarc -> ehlo -> headers -> received -> messageid -> date -> subject
      -> from -> replyto -> recipient -> domain -> url -> mime -> html
      -> spam-trap -> pyzor -> classifier -> rules -> finalize

Implemented analyzers: SPF/DKIM/DMARC and ARC chain results; forward-confirmed
reverse DNS and HELO sanity; peer-IP and domain DNSBL/RBL (answers cached in
Redis); a Pyzor fuzzy digest; header, body, Received, URL and attachment
heuristics (including a forged Received check against the peer IP); spam traps; a
declarative rules DSL; and an online logistic-regression classifier
(FTRL-Proximal over signed, feature-hashed tokens).

Classifier confidences become `PROB_SPAM_*` / `PROB_HAM_*` tags. The classifier
learns from user feedback (moving mail into or out of the spam folder) and
from spam traps / DNSBL hits once it has enough samples.

ClamAV (INSTREAM), olefy (Office macros) and Tika (text extraction) run when
their endpoints are configured. A `MALWARE_*` hit forces quarantine whatever
the score says. Every network analyzer fails open: if Redis, the resolver or a
scanner is down, the message is delivered and the failure logged.

## Configuration

`SPAM_CONFIG_PATH` (default `/etc/copod-mail/spam.json`) holds the thresholds,
the tag -> score map, DNSBL zones, traps, dangerous extensions, spam words,
rules and classifier parameters. See `config/spam.example.json`. Switches and
endpoints also come from the environment; see `.env.example`.

## Deployment

The engine needs Redis once enabled. `compose.yml` starts Redis and ClamAV for
local development; the k3s manifests live in the infrastructure repository.
