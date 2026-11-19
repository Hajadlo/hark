# hark API

The contract between the server (`server/`) and its clients: the Linux client
(`linux/`), the Android app (`android/`) and the web page
(`server/web/index.html`).

## Base URL

`https://<host>.<tailnet>.ts.net:8766` through Tailscale Serve; the gateway
itself listens on `127.0.0.1:8766`. Clients store the base URL without `/v1`,
strip a trailing `/` or `/v1` the user pastes, and append the paths below.

## Authentication

| Route | Bearer token | Owner login |
|---|---|---|
| `GET /health` | not needed | not needed |
| `POST /v1/audio/transcriptions` | yes | no |
| `GET /v1/models` | yes | yes |
| `GET /v1/history` | yes | yes |
| `GET /v1/corrections` | yes | yes |
| `GET`, `POST`, `DELETE /v1/glossary` | yes | yes |
| `GET /` (web page) | no | yes |

- **Bearer token:** `Authorization: Bearer <token>`, the `api-token` file in the
  state directory (at least 32 characters). The apps use this.
- **Owner login:** the `Tailscale-User-Login` header equals the owner login
  (`VOICE_GATEWAY_OWNER`, default `owner@example.com`), compared
  case-insensitively, the same check as `~/.local/bin/private-http`. Tailscale
  Serve sets this header on requests from tailnet users. The web page uses
  this. An owner-authenticated `/v1/*` request must also:
  - carry `X-Hark-Web: 1`;
  - have no `Origin` header, or `Origin: https://<its Host header>`;
  - use `Content-Type: application/json` when it has a body.

  These force a CORS preflight that the server never approves, so another site
  open in the user's browser cannot use the API through Serve.
- Failure: `401` on `/v1/*`, `403` on `/`.

## Errors

Every non-2xx JSON response is `{"error": {"message": "<readable text>"}}`.
Clients show the message.

| Status | Meaning | Client action |
|---|---|---|
| 400 | malformed request or parameter | show, no retry |
| 401, 403 | credentials | show "check the token", no retry |
| 404 | unknown route | show, no retry |
| 411 | no `Content-Length` | client bug |
| 413 | upload over 64 MiB | show, no retry |
| 422 | unreadable audio, no audio, or over 600 s | show, no retry |
| 503 | another recording is still processing after 60 s | retry |
| other 5xx, network error, timeout | | retry |

## POST /v1/audio/transcriptions

OpenAI's transcription request shape, so third-party clients work too.

**Request:**

- `Content-Type: multipart/form-data` with a `Content-Length` (no chunked
  encoding).
- `X-Hark-Device`: `desktop`, `laptop`, `phone`, or `test` in automated tests.
  Stored lowercased when it matches `^[a-z0-9-]{1,32}$`, otherwise and when
  missing as `unknown`.
- `User-Agent`: `hark-linux/<version>` or `hark-android/<version>`; stored as
  `client`, first 80 characters.
- Form fields:
  - `file` (required): the audio. hark clients send WAV, 16 kHz, mono, 16-bit
    PCM, filename `audio.wav`, part type `audio/wav`. The server decodes
    anything FFmpeg reads.
  - `model` (optional, default `clean`): a mode name (see [Modes](#modes)).
    Unknown names, such as OpenAI's `whisper-1`, run `clean`.
  - `response_format` (optional): `json` (default) or `text`.

**Response `200`:**

- `json`: `{"text": "Open the PR.", "id": 42, "mode": "clean", "cleanup": "ok", "language": "en"}`.
  Clients need only `text`. `id` is the history entry, `null` when nothing was
  heard. `mode` is the mode that ran. `language` is the language the recognizer
  used (see [Languages](#languages)), `null` when the model reports none.
- `text`: the text alone as `text/plain; charset=utf-8`.
- Nothing heard: `text` is `""` and nothing is stored.

`cleanup` is `off` (a mode without a prompt), `ok`, `failed` (the LLM failed;
the raw text is returned) or `rejected` (the drift guard tripped; the raw text
is returned).

### Languages

The recognizer is set by `VOICE_GATEWAY_ASR_MODEL`. A multilingual model
(Whisper) detects the language of each recording on its own; there is no
per-language key or mode. `VOICE_GATEWAY_LANGUAGES` (default `en,cs`) limits
the result to those languages: a recording detected as anything else is
transcribed in the closest allowed language. Whisper often labels short
English with Czech names as Czech, so when English is allowed and the label
is not English, the recording is also decoded as English and the more
confident transcript wins, with a small lean toward English. A recording
labelled English is decoded once. Once Czech is chosen, the Czech audio is
decoded again with a hint of common Czech dev verbs and nouns (pushni,
mergni, do mainu, …) and the glossary terms, so slang and names come out
right. The hint never affects the language choice or capitalization, and
when the glossary outgrows Whisper's prompt limit, its oldest terms are left
out of the hint (cleanup still uses them all). With
`VOICE_GATEWAY_ENGLISH_HINT=1`, an English result gets the same treatment: a
second pass with the glossary as a Whisper prompt. Only its one-for-one
respellings are kept: a heard word into a glossary word that sounds like it,
with the first transcript's punctuation. Any other difference, merges such as
"bad boom" into "Kubernetes" included, keeps the first transcript's words;
cleanup and its heard-as hints handle merges. An English-only model ignores the
setting and reports no language.

A recording without speech (silence, room noise, a click) returns `""`, the
same as "Nothing heard", and is not stored, whichever model runs.

**Limits:** 64 MiB upload, 600 s of audio, one recording processed at a time; a
waiting request gets `503` after 60 s. Clients use a read timeout of at least
90 s. The server never writes audio to disk.

## Modes

`<state>/modes.toml`, read with `tomllib` on every request. Each top-level
table is a mode; the starter file is `server/modes.toml`.

```toml
[clean]
description = "Punctuation, numbers, no filler words"
prompt = """You clean up dictated text. ..."""

[raw]
description = "As heard"

[email]                 # an example user mode
description = "Polite email"
prompt = "..."
drift_guard = false     # default true
```

- A mode without `prompt` runs no cleanup.
- The glossary sentence is appended to every prompt.
- Cleanup keeps each word in the language spoken: the starter `clean` prompt
  says so, because without it the model translated Czech into English.
- `drift_guard` (default `true`) returns the raw text when the output length
  falls outside 0.4–1.3 × the transcript (+20 characters), as `clean` does today.
  It also keeps accents: a word whose diacritics the cleanup removed or
  changed is put back as transcribed; added accents and glossary corrections
  stay.
- Mode names match `^[a-z0-9-]{1,32}$`; other tables are skipped with a logged
  warning.
- `clean` (today's `PROMPT`) and `raw` always exist: the built-ins fill in when
  the file omits them or is missing.
- A file that fails to parse leaves the last valid version in force (or the
  built-ins) and logs a warning; transcription never fails because of it.

## GET /v1/models

```json
{"object": "list", "data": [
  {"id": "clean", "object": "model", "owned_by": "hark", "description": "Punctuation, numbers, no filler words", "cleanup": true},
  {"id": "raw", "object": "model", "owned_by": "hark", "description": "As heard", "cleanup": false}
]}
```

Order: the file's order, then any built-in the file omits.

## History

Each dictation with a non-empty transcript becomes an entry in SQLite
(`<state>/history.sqlite3`, with an FTS5 index over `raw` and `text`).

```json
{
  "id": 42,
  "time": "2026-10-11T16:20:31Z",
  "device": "desktop",
  "client": "hark-linux/0.1.0",
  "mode": "clean",
  "audio_seconds": 2.5,
  "asr_seconds": 0.051,
  "cleanup_seconds": 0.412,
  "cleanup": "ok",
  "language": "en",
  "raw": "so um open the pr",
  "text": "Open the PR."
}
```

`time` is UTC, ISO 8601 to the second, with `Z`. `language` is `null` for
entries made by an English-only model and for imported entries.

### GET /v1/history

Query parameters, all optional:

- `q`: free text. The server splits it into words and returns entries whose
  `raw` or `text` contains every word as a word prefix, so `clau cod` finds
  "Claude Code". Quotes, `*`, `-`, `:` and other FTS5 syntax in `q` are plain
  text.
- `limit`: 1–200, default 50.
- `before`: an entry id; only older entries (smaller ids) are returned.
- `device`, `mode`: exact filters.

Response: `{"data": [<entry>, ...], "next_before": 17}`, newest first (by
`id`). Pass `next_before` as `before` for the next page; it is `null` on the
last page. A bad parameter is `400`.

### Import

At startup, if `<state>/history.jsonl` exists, its records are imported in one
transaction: `timestamp` → `time`, `pipeline` → `mode` (resolved like a
request's `model`), `client`, `audio_seconds`, `asr_seconds`,
`cleanup_seconds`, `cleanup`, `raw`, `text`, and `device` = `unknown`. The file
is then renamed to `history.jsonl.imported`. The server writes no JSONL after
that.

## GET /v1/corrections

Word-level substitutions the cleanup made across the history, so a recurring
mishearing can become a glossary term with one click.

- **Source:** entries with `cleanup` = `ok`.
- **Method:** `difflib.SequenceMatcher` over the word lists of `raw` and
  `text`, comparing words with case and surrounding punctuation ignored. Each
  `replace` block of at most 4 words per side is one substitution.
- **Skipped:** `equal`, `insert` and `delete` blocks; blocks that differ only in
  case or punctuation; blocks whose final side has no letter (`sixty six` →
  `66`).
- **Reported as:** `raw` from the raw words and `final` from the final words,
  with punctuation trimmed from both ends, grouped by (`raw` lowercased,
  `final`). Equal neighbouring words stay out, so `clod code` → `Claude Code`
  reports `clod` → `Claude`.

Query: `limit` 1–500 (default 100), `min_count` (default 1).

```json
{"data": [
  {"raw": "clod", "final": "Claude", "count": 3, "last_id": 41, "last_time": "2026-10-11T16:20:31Z", "in_glossary": false}
], "entries_scanned": 812}
```

Sorted by `count` descending, then `last_id` descending. `in_glossary` is true
when `final` equals a glossary term, case-insensitively.

## Glossary

`<state>/glossary.txt`: one term per line, `#` comments, re-read on every
request. A line may add how the recognizer mishears the term after a colon
and a space: `skald: sculpt, kalt` (`Re:Zero` stays one term). Cleanup is then told those forms mean the term; the
term alone is what the rest of the API sees.

- `GET /v1/glossary` → `{"terms": ["Claude Code", ...], "hints": {"skald":
  ["sculpt", "kalt"]}}`, terms in file order, `hints` only for lines that have
  them.
- `POST /v1/glossary`, body `{"term": "Claude Code"}` → appends the term unless
  it is already there (case-insensitive) → `{"terms": [...], "added": true}`
  (or `false`).
- `DELETE /v1/glossary?term=<url-encoded>` → removes the matching lines
  (case-insensitive, matching the part before any colon) → `{"terms": [...], "removed": true}` (or `false`).
- A term is trimmed and must be 1–64 characters on one line, with no control
  character, no comma, no colon followed by a space and no leading `#`;
  otherwise `400`. Hints are
  added by editing the file.
- A write keeps comments, blank lines and order, and replaces the file
  atomically (a temp file in the same directory, then rename).

## Web page

`GET /` serves `web/index.html` from the directory of the resolved
`voice_gateway.py`: one file with inline CSS and JS, no build step, owner login
only. Response headers:

- `Content-Type: text/html; charset=utf-8`
- `Cache-Control: no-store`
- `Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`
- `Referrer-Policy: no-referrer`
- `X-Content-Type-Options: nosniff`

The page calls `/v1/*` with `X-Hark-Web: 1` and no token. It offers:

- **History:** newest first, a search box, paging with `next_before`. Each
  entry shows local time, device, mode and the final text, the raw text on
  demand, a Copy button for the final text, and "Add to glossary" for selected
  text or a typed term.
- **Corrections:** the `/v1/corrections` list, with an Add button per row,
  disabled when `in_glossary`.
- **Glossary:** the terms, each with Remove.

It works on a phone screen and in a desktop browser.

## Shared client behaviour

- **One key, two gestures.** A press starts recording at once.
  - Released within 300 ms: toggle. Recording continues until the next press
    of any hark shortcut, which stops it.
  - Held 300 ms or longer: hold. Recording stops on release.
  - Recording stops by itself at 10 minutes.
  - The mode chosen at the press that started the recording applies.
- **Bindings.** Linux: Ctrl+Space → `clean`, Ctrl+Shift+Space → `raw`,
  configurable. Android: the bubble, mode chosen in settings.
- **Busy.** While a request is in flight, a press is ignored with a short busy
  signal.
- **Upload.** The WAV is kept (in memory on Linux, as an app cache file on
  Android) until a `200`. Up to 3 attempts in total, about 1 s and 2 s apart,
  on network errors, timeouts, `503` and other 5xx. After the last failure the
  user can retry or discard. The recording is deleted on success or discard.
  Linux holds only the latest failed recording, in memory: starting a new
  recording replaces it. Android keeps each failed recording as a cache file
  until it is retried or discarded.
- **Deadlines.** An upload, including writing the request body, has an overall
  deadline (about 120 s); a stalled connection counts as a network error.
- **Insert** `text` at the cursor:
  - Linux pastes: save the clipboard, `wl-copy` the text, Ctrl+V through
    uinput, restore the clipboard.
  - Android calls `commitText` through the accessibility input connection,
    falling back to `ACTION_SET_TEXT` plus a selection.
  - The clipboard ends as it started.
- **Empty text:** insert nothing; give a short "nothing heard" signal.
- **The token** never appears in logs, notifications or crash output.
