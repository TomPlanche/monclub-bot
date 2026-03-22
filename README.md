# monclub-bot

CLI bot that books and cancels sessions on a MonClub-powered platform.

The MonClub app does not expose a public API. See [REVERSE_ENGINEERING.md](./REVERSE_ENGINEERING.md) for how the API was discovered by intercepting the app's HTTPS traffic.

## Configuration

```bash
cp .env.example .env
```

| Variable         | Required | Default | Description                                      |
|------------------|----------|---------|--------------------------------------------------|
| `EMAIL`          | yes      |         | Account email                                    |
| `PASSWORD`       | yes      |         | Account password                                 |
| `CUSTOM_ID`      | yes      |         | Club identifier                                  |
| `BASE_URL`       | yes      |         | API base URL (no trailing slash)                 |
| `LATITUDE`       | no       |         | Latitude sent to the API (affects proximity sorting)  |
| `LONGITUDE`      | no       |         | Longitude sent to the API (affects proximity sorting) |
| `RETRY_DURATION` | no       | `300`   | Total seconds to keep retrying before giving up       |
| `RETRY_INTERVAL` | no       | `5`     | Seconds between retries                          |

## Usage

```
cargo run
```

The bot will:
1. Authenticate
2. Ask whether to **book** or **cancel**

**Book**
1. Fetch all club sessions
2. Present a selection prompt
3. Ask for confirmation
4. Submit the booking, retrying on 409 until the slot opens or the deadline is reached

**Cancel**
1. Fetch the user's upcoming bookings via `/bookings/user/<userId>`
2. Present a selection prompt
3. Ask for confirmation
4. Submit the cancellation

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

| Param                    | Value  |
|--------------------------|--------|
| `withCoachAuthentication`| `true` |

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

| Param      | Value        |
|------------|--------------|
| `customId` | `<CUSTOM_ID>`|
| `userId`   | `<user_id>`  |

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

| Param         | Value         |
|---------------|---------------|
| `category`    | `ondemand`    |
| `temporality` | `fromToday`   |

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

### Book

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

| Code  | Meaning                                            |
|-------|----------------------------------------------------|
| `2xx` | Booking confirmed                                  |
| `409` | Slot not open yet — bot retries until the deadline |
| other | Fatal error — bot exits                            |

### Cancel

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

| Code  | Meaning                         |
|-------|---------------------------------|
| `2xx` | Cancellation confirmed          |
| other | Fatal error — bot exits         |
