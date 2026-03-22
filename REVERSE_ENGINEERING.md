# Reverse Engineering the MonClub App

The MonClub app does not expose a public API. To automate session booking, I intercepted the app's HTTPS traffic using [Proxyman](https://proxyman.io/) on macOS with an iPhone as the client. Proxyman decrypts TLS traffic by acting as a man-in-the-middle proxy after its CA certificate is trusted on the device.

All raw captures are saved under `./assets/`:

| File | What it contains |
|------|-----------------|
| [`assets/post_all_events.folder/`](./assets/post_all_events.folder/) | `POST /nearfilters/favorite/myclub` — request and response for listing all club sessions |
| [`assets/book_session.folder/`](./assets/book_session.folder/) | `POST /sessions/book/licenseeFromClub` — request and response for booking a session |
| [`assets/cancel_session.folder/`](./assets/cancel_session.folder/) | `POST /sessions/book/licenseeFromClub` — request and response for cancelling a booking |
| [`assets/post_all_events.har`](./assets/post_all_events.har) | Full HAR export of the session listing flow |

## Key IDs

Every request involves a set of opaque MongoDB ObjectIds that had to be extracted from the traffic:

| ID | Where found | What it is |
|----|-------------|------------|
| `userId` | Auth response body (`userId` or `user._id`) | Identifies the logged-in user |
| `customId` | Every request — body or query param | Identifies the club/tenant |
| `sessionId` | Session listing response (`_id`) | Identifies a specific session slot |
| `bookingId` | Bookings listing response (`_id`) | Identifies a booking record, required to cancel |

`customId` is stable and never changes for a given club. The others are fetched at runtime.

## Auth Flow

The traffic revealed a two-step authentication flow:

**Step 1** — `POST /users/custom/authenticate/email/v2`: sends only the email address. The server likely checks whether the account exists and what login method to use.

**Step 2** — `POST /users/custom/authenticate/v2`: sends credentials plus device metadata. Returns a raw JWT (no `Bearer` prefix) and the `userId`.

The JWT is then sent as `Authorization: <token>` on all subsequent requests. Its expiry is ~1 year, so re-authenticating on every run is safe without needing token refresh logic.

The device metadata in the request body is hardcoded — the server accepts arbitrary values:
```json
{
    "os":      "Android 14",
    "model":   "Phone (2)",
    "brand":   "Nothing",
    "version": "3.6.0",
}
```

## Discovered Endpoints

### Session listing — [`assets/post_all_events.folder/`](./assets/post_all_events.folder/)

`POST /nearfilters/favorite/myclub` returns all upcoming sessions for the user's clubs. The `tagName: "myclub"` filter scopes the results to clubs the user is a member of. Found by capturing traffic while opening the "Sessions" tab in the app.

### Bookings listing

`GET /bookings/user/:userId?category=ondemand&temporality=fromToday` returns the user's upcoming bookings. Each entry contains a nested `session` array with the session details and a top-level `_id` — that `_id` is the `bookingId` required for cancellation. Found by capturing traffic while opening the "My Bookings" tab.

### Booking and cancellation — [`assets/book_session.folder/`](./assets/book_session.folder/) · [`assets/cancel_session.folder/`](./assets/cancel_session.folder/)

`POST /sessions/book/licenseeFromClub` handles both operations. The `isPresent` field in the participant object controls which:

- `"yes"` — creates a booking
- `"no"` — cancels a booking; also requires `bookingId` in the participant object

Found by tapping "Book" and "Cancel" in the app and observing the resulting requests.

## Notes

- `409 Conflict` on the booking endpoint means the slot is not yet open. This is expected for sessions that open at a specific time, which is why the bot retries on 409.
- The backend is hosted on Heroku (`BASE_URL`). The iOS client identifies itself with the User-Agent `MonClubFakeAgent/311 CFNetwork/3860.500.83 Darwin/25.4.0`.
- All IDs are MongoDB ObjectIds (24-character hex strings).
