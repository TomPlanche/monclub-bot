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
| `DISCORD_OWNER_ID` | no | Your Discord user ID. When set, only that user can trigger commands. |

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
2. Prompts to **book** or **cancel**

**Book**
1. Fetches all upcoming club sessions
2. Presents a selection prompt
3. Asks for confirmation
4. Submits the booking, retrying on 409 until the slot opens or the deadline is reached

**Cancel**
1. Fetches the user's upcoming bookings
2. Presents a selection prompt
3. Asks for confirmation
4. Submits the cancellation

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
| `/book <session>` | Book a session. The `session` argument has autocomplete — type to filter, or paste an ID from `/list`. If the slot returns 409 (not open yet), a background task retries and sends a follow-up message when confirmed or the deadline is hit. |
| `/cancel <booking>` | Cancel an upcoming booking. The `booking` argument has autocomplete. |
| `/bookings` | List your upcoming bookings. |

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

Returns the authenticated user's upcoming bookings.

**Query params**

| Param | Value |
|-------|-------|
| `category` | `ondemand` |
| `temporality` | `fromToday` |

**Response**: a JSON array of booking objects. Session details are nested inside the `session` array.

```json
[
  {
    "_id":       "<booking_id>",
    "sessionId": "<session_id>",
    "session": [
      {
        "_id":         "<session_id>",
        "sessionName": "Jeu Libre",
        "date":        "2026-03-28T16:00:00.000Z",
        "time":        "17H00"
      }
    ]
  }
]
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
