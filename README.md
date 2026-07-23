# monclub-bot

Books and cancels sessions on a MonClub-powered platform.
Available as a terminal CLI and a Discord slash-command bot.

The MonClub app does not expose a public API. See [REVERSE_ENGINEERING.md](./REVERSE_ENGINEERING.md) for how the API was discovered by intercepting the app's HTTPS traffic.

## Binaries

| Binary | Command | Description |
|--------|---------|-------------|
| `monclub-bot` | `cargo run --release` | Interactive terminal CLI (default) |
| `monclub-discord` | `cargo run --release --features discord --bin monclub-discord` | Discord slash-command bot |

The `discord` feature is opt-in. Without it, `poise` and `tokio` are not compiled, keeping the CLI build fast and lean.

## Running

### CLI

```bash
# Development
cargo run

# Release (recommended)
cargo run --release

# Pre-built binary
cargo build --release
./target/release/monclub-bot
```

### Discord bot

```bash
# Development
cargo run --features discord --bin monclub-discord

# Release (recommended)
cargo run --release --features discord --bin monclub-discord

# Pre-built binary
cargo build --release --features discord
./target/release/monclub-discord
```

### Build both at once

```bash
cargo build --release --features discord
```

---

## Configuration

```bash
cp .env.example .env
```

### Core (required by both binaries)

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `EMAIL` | yes | | Account email |
| `PASSWORD` | yes | | Account password |
| `CUSTOM_ID` | yes | | Club identifier |
| `BASE_URL` | yes | | API base URL (no trailing slash) |
| `LATITUDE` | no | | Latitude sent to the API (affects proximity sorting) |
| `LONGITUDE` | no | | Longitude sent to the API (affects proximity sorting) |
| `RETRY_DURATION` | no | `300` | Total seconds to keep retrying before giving up |
| `RETRY_INTERVAL` | no | `5` | Seconds between retries |

### Discord bot

| Variable | Required | Description |
|----------|----------|-------------|
| `DISCORD_TOKEN` | yes | Bot token from the [Discord Developer Portal](https://discord.com/developers/applications) |
| `DISCORD_OWNER_ID` | no | Your Discord user ID. When set, only that user can trigger commands, and it links the owner to the primary `EMAIL`/`PASSWORD` account (unless overridden by a `users.json` entry — see below). |
| `BOOKING_WINDOW_HOURS` | no | Hours before a session's start that its booking window opens. Used by `/notify` and `/watchbook` to work out when a not-yet-bookable session becomes bookable. Defaults to `144` (6 days). |
| `WATCH_POLL_INTERVAL` | no | Seconds between `/watchbook` retries once the booking window is open but the session is full, i.e. how often it checks whether somebody has unbooked. Deliberately much slower than `RETRY_INTERVAL`, since this watch can run for days. Defaults to `60`. |

#### Multi-user (`users.json`)

Extra bookable accounts, so booking/cancelling can act for several people at once, are configured in a `users.json` file in the working directory (next to `.env`) — **not** in the environment. The file is optional; without it, only the primary account is bookable. It contains credentials, so it is gitignored. Copy `users.json.example` to get started.

It is a JSON array; each entry has its own `email`/`password`/`label` and the `discord_id` it maps to. `custom_id` is optional and defaults to `CUSTOM_ID`. An entry **takes precedence over `EMAIL`/`PASSWORD`** when it reuses the owner's `discord_id` (or the label `me`), letting you override the primary account without editing it.

```json
[
  {"discord_id": 123, "label": "tom", "email": "tom@x.com", "password": "pwd"}
]
```

Both interfaces use this file:

- **CLI** — `book`, `prebook`, and `manage` (the cancel action) take a `--for <labels>` flag with comma-separated `label`s (e.g. `--for me,tom`). Without the flag, they prompt you to multi-select accounts **only when `users.json` has entries**; a single-account setup keeps its current, promptless behaviour. Each person is booked/cancelled under their own account, with a per-account outcome.
- **Discord** — accounts are keyed by `discord_id`; `/book` and `/cancel` take a `users` argument (mentions, raw ids, or labels). See the command table below.

### Logging

| Variable | Default | Description |
|----------|---------|-------------|
| `RUST_LOG` | `info` | Log level filter. Accepts `error`, `warn`, `info`, `debug`, `trace`, or crate-specific directives (e.g. `monclub_bot=debug`). |

Logs are written to `logs/<binary-name>.YYYY-MM-DD.log` (daily rotation, 30-day retention). The Discord bot also mirrors logs to stdout for use with systemd or Docker.

## Usage

### CLI

```
cargo run --bin monclub-bot
```

1. Authenticates against the API
2. Prompts to choose an action from the interactive menu

All session and booking pickers display four columns: name, date, time, and current/total participants.

**Book**
1. Fetches upcoming club sessions (excluding ones you have already booked)
2. Presents a selection prompt
3. Chooses who to book for: the `--for` labels, or a multi-select prompt when `users.json` has entries (skipped for single-account setups)
4. Asks for confirmation
5. Submits the booking for each account, retrying on 409 until the slot opens or the deadline is reached

**Schedule a booking (`prebook`)**
1. Picks a session (interactive or by ID) and who to book for (`--for` or prompt)
2. Asks for a target time (`HH:MM` or `YYYY-MM-DD HH:MM`)
3. Sleeps until that time, then runs the same retry loop as Book for each account

**View / manage bookings**
1. Fetches the user's upcoming bookings
2. Presents a selection prompt
3. Asks what to do with the selected booking:
   - **View info** — fetches and displays full session detail (location, capacity, coaches, participants list, etc.)
   - **Cancel reservation** — chooses who to cancel for (`--for` or prompt), asks for confirmation, then cancels each account's own booking for that session

**See previous sessions**
1. Fetches the user's past bookings, sorted most recent first
2. Presents a selection prompt
3. Fetches and displays full session detail for the selected entry

**Compare participants**
1. Prompts for two sessions (interactive or by ID; booked sessions are sorted first)
2. Displays three sets: participants in both, only in the first, only in the second

### Discord bot

```
cargo run --release --features discord --bin monclub-discord
```

Slash commands are registered globally on first startup (may take up to an hour to propagate, but is usually instant in your own server).

#### Setup

1. Create an application and bot at [discord.com/developers/applications](https://discord.com/developers/applications)
2. Copy the bot token into `DISCORD_TOKEN` in `.env`
3. Enable **Developer Mode** in Discord settings, then right-click your username and select **Copy User ID**; set it as `DISCORD_OWNER_ID`
4. Invite the bot to your server with the `bot` and `applications.commands` scopes

#### Commands

| Command | Description |
|---------|-------------|
| `/list [limit]` | List available sessions sorted by date, each with its ID. Optional `limit` restricts to the first N results. Long lists are split across multiple messages automatically. |
| `/book <session> [users]` | Book a session. The `session` argument has autocomplete — type to filter, or paste an ID from `/list`. `users` books for other linked people at once — space-separated mentions (`@tom @nils`), raw Discord ids, or `users.json` labels, or `@everyone` (also `everyone`/`all`) for every configured account; omit it to book only for yourself. **Single target:** if the slot returns 409 (not open yet), a background task retries and posts a follow-up when confirmed or the deadline is hit. **Multiple targets:** the group is booked **atomically** — if any account fails (no credits, error, or slot not open), the bookings that already succeeded are cancelled (rolled back) so nobody is left half-booked. Use `/prebook` to schedule a group for when the slot opens. |
| `/booking <booking>` | Show full detail for one of your bookings: session name, date/time, location, participant count, coaches, description, and numbered participants list. The `booking` argument has autocomplete. |
| `/cancel <booking> [users]` | Cancel an upcoming booking. The `booking` argument has autocomplete. `users` cancels for other linked people at once (same syntax as `/book`); each person's own booking for that session is looked up and cancelled. |
| `/prebook <session> <when> [users]` | Schedule a booking to fire at a given time (`HH:MM` or `YYYY-MM-DD HH:MM`), optionally for other linked people. |
| `/bookings` | List your upcoming bookings. |
| `/notify <session>` | Get pinged in the channel when a session that isn't bookable yet crosses its booking window (`session start − BOOKING_WINDOW_HOURS`, default 144h). The `session` argument has autocomplete. Alert only — it does not book; you book it yourself in the app. If the session is already bookable it says so immediately. Like `/prebook`, the pending alert is in-memory and lost if the bot restarts. |
| `/watchbook <session> [users]` | `/notify` + `/book`: watch a session and book it the moment it can actually be booked, then post the outcome in the channel with a ping. `users` books for other linked people at once (same syntax as `/book`). It waits through **both** reasons a session can't be booked yet: (1) the booking window not being open (`session start − BOOKING_WINDOW_HOURS`), retried every `RETRY_INTERVAL` seconds for `RETRY_DURATION` once the window opens, since the API can still answer 409 right at the boundary; and (2) the session being **full**, retried every `WATCH_POLL_INTERVAL` seconds (default 60) until the session starts, so a spot freed by somebody unbooking is claimed automatically. It books by attempting the booking rather than by reading free capacity, so it claims the spot in the same request that detects it. Rejections that waiting can't fix (`noCredits`, `noMembership`) stop the watch immediately; any other status is treated as "still full" and reported once so an unexpected one stays visible. Unlike `/book` with several targets, this is **not atomic** — each account is watched independently and a failure for one does not roll back the others. Like `/prebook`, the pending watch is in-memory and lost if the bot restarts. |

#### Running as a service (systemd example)

```ini
[Unit]
Description=monclub Discord bot
After=network-online.target

[Service]
WorkingDirectory=/path/to/monclub-bot
ExecStart=/path/to/monclub-bot/target/release/monclub-discord
Restart=on-failure
RestartSec=10
EnvironmentFile=/path/to/monclub-bot/.env

[Install]
WantedBy=multi-user.target
```

---

## API Endpoints

All requests are sent to `BASE_URL` with the following headers:

```
Content-Type:    application/json
Accept:          application/json
Accept-Language: fr
User-Agent:      okhttp/4.12.0
```

Authenticated requests additionally carry:

```
authorization: <raw JWT token>
```

---

### POST /users/custom/authenticate/email/v2

Email probe — step 1 of the two-step auth flow.

**Query params**

| Param | Value |
|-------|-------|
| `withCoachAuthentication` | `true` |

**Request body**

```json
{
  "email": "your@email.com"
}
```

**Response**: not used; only the status code is checked.

---

### POST /users/custom/authenticate/v2

Full authentication — step 2. Returns the session token.

**Request body**

```json
{
  "credentials": {
    "email": "your@email.com",
    "password": "yourpassword"
  },
  "customId": "<CUSTOM_ID>",
  "deviceInfo": {
    "os":      "Android 14",
    "model":   "Phone (2)",
    "brand":   "Nothing",
    "version": "3.6.0"
  },
  "coachAuthentication": false
}
```

**Response** (relevant fields)

```json
{
  "token": "<JWT>",
  "userId": "<user_id>"
}
```

`userId` may alternatively be nested as `user._id`.

---

### POST /nearfilters/favorite/myclub

Lists all upcoming sessions for the user's clubs.

**Query params**

| Param | Value |
|-------|-------|
| `customId` | `<CUSTOM_ID>` |
| `userId` | `<user_id>` |

**Request body**

```json
{
  "filters": {
    "tagName":     "myclub",
    "coordinates": [2.3376446, 48.8704031],
    "price":       null,
    "discipline":  null,
    "date":        null,
    "time":        null,
    "level":       null,
    "type":        null,
    "category":    null,
    "pinnedSlots": null,
    "categoryId":  null,
    "group":       null
  },
  "coordinates": [2.3376446, 48.8704031]
}
```

`coordinates` is `[longitude, latitude]`. Both fields are optional and can be `null`; when provided they affect proximity sorting.

**Response**: a JSON array of session objects. Non-array responses are treated as an empty list.

```json
[
  {
    "_id":         "<session_id>",
    "sessionName": "LOISIR Supplémentaire - Samedi Lacretelle- 17h-19h",
    "date":        "2026-03-28T16:00:00.000Z",
    "time":        "17H00",
    "type":        "free",
    "discipline":  "volley"
  }
]
```

---

### GET /bookings/user/:userId

Returns the authenticated user's bookings.

**Query params**

| Param | Value |
|-------|-------|
| `category` | `ondemand` |
| `temporality` | `fromToday` for upcoming bookings, `beforeToday` for past bookings |

**Response**: a JSON array of booking objects. Session details are nested inside the `session` array.

```json
[
  {
    "_id":       "<booking_id>",
    "sessionId": "<session_id>",
    "session": [
      {
        "_id":               "<session_id>",
        "sessionName":       "Jeu Libre",
        "date":              "2026-03-28T16:00:00.000Z",
        "time":              "17H00",
        "yesParticipants":   ["<user_id>", "..."],
        "totalQuantityFree": 24
      }
    ]
  }
]
```

---

### POST /sessions/withuser

Fetches full detail for a single session, including the attendees list.

**Request body**

```json
{
  "sessionId": "<session_id>",
  "userId":    "<user_id>"
}
```

**Response** (relevant fields, wrapped in a `session` key)

```json
{
  "session": {
    "_id":               "<session_id>",
    "sessionName":       "Session name",
    "date":              "2026-04-04T15:00:00.000Z",
    "time":              "17H00",
    "endTime":           "19H00",
    "place": {
      "name":    "Venue name",
      "address": "1 rue Example",
      "zipCode": "75000",
      "city":    "Paris"
    },
    "totalQuantityFree": 24,
    "price":             5,
    "level":             "allLevels",
    "description":       "Jeu libre",
    "info":              "Sur inscription préalable",
    "coachs": [
      { "fullName": "Coach Name" }
    ],
    "yesParticipants": ["<user_id>", "..."],
    "attendees": [
      {
        "userId":     "<user_id>",
        "fullName":   "User Name",
        "memberNumber": 1001
      }
    ]
  },
  "accessToTrialSession": false
}
```

---

### POST /sessions/book/licenseeFromClub

Books or cancels a session for the authenticated user. The same endpoint is used for both operations; the `isPresent` field distinguishes them.

#### Book

**Request body**

```json
{
  "participant": {
    "userId":      "<user_id>",
    "isPresent":   "yes",
    "coordinates": null
  },
  "sessionId": "<session_id>",
  "customId":  "<CUSTOM_ID>"
}
```

**Status codes**

| Code | Meaning |
|------|---------|
| `2xx` | Booking confirmed |
| `409` | Slot not open yet — bot retries until the deadline |
| other | Fatal error — bot exits |

#### Cancel

**Request body**

```json
{
  "participant": {
    "userId":      "<user_id>",
    "isPresent":   "no",
    "coordinates": null,
    "bookingId":   "<booking_id>"
  },
  "sessionId": "<session_id>",
  "customId":  "<CUSTOM_ID>"
}
```

`bookingId` is the `_id` returned by `GET /bookings/user/:userId`.

**Status codes**

| Code | Meaning |
|------|---------|
| `2xx` | Cancellation confirmed |
| other | Fatal error — bot exits |
