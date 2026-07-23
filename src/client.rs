use std::collections::HashSet;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, NaiveDateTime, NaiveTime};
use inquire::tabular::{ColumnAlignment, ColumnConfig};
use inquire::{Confirm, MultiSelect, Select, Text};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tracing::{info, warn};

use crate::config::{Account, Config, same_identity};

#[derive(Debug, Deserialize)]
struct AuthResponse {
    token: Option<String>,
    #[serde(rename = "userId")]
    user_id: Option<String>,
    user: Option<AuthUser>,
}

impl AuthResponse {
    fn resolve_user_id(&self) -> Option<&str> {
        self.user_id
            .as_deref()
            .or_else(|| self.user.as_ref().map(|u| u.id.as_str()))
    }
}

#[derive(Debug, Deserialize)]
struct AuthUser {
    #[serde(rename = "_id")]
    id: String,
}

#[derive(Debug, Deserialize)]
pub struct Session {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "sessionName")]
    pub name: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    #[serde(rename = "yesParticipants", default)]
    pub yes_participants: Vec<String>,
    #[serde(rename = "totalQuantityFree")]
    pub total_capacity: Option<u32>,
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let date = self
            .date
            .as_deref()
            .and_then(|d| d.get(..10))
            .unwrap_or("?");
        let participants = match self.total_capacity {
            Some(cap) => format!("{}/{cap} booked", self.yes_participants.len()),
            None if !self.yes_participants.is_empty() => {
                format!("{} booked", self.yes_participants.len())
            }
            None => "?".to_string(),
        };
        write!(
            f,
            "{} | {} | {} | {}",
            self.name.as_deref().unwrap_or(&self.id),
            date,
            self.time.as_deref().unwrap_or("?"),
            participants,
        )
    }
}

impl Session {
    /// Chronological ordering key. `date` is an ISO timestamp, `time` a zero-padded `19H30`, so both sort correctly as plain strings. Entries missing a date sort last rather than silently jumping to the top.
    fn sort_key(&self) -> (bool, &str, &str) {
        (
            self.date.is_none(),
            self.date.as_deref().unwrap_or(""),
            self.time.as_deref().unwrap_or(""),
        )
    }

    /// A minimal `Session` carrying only its id, for booking by id without
    /// having fetched the full listing.
    pub fn from_id(id: String) -> Self {
        Self {
            id,
            name: None,
            date: None,
            time: None,
            yes_participants: vec![],
            total_capacity: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BookingSession {
    #[serde(rename = "sessionName")]
    pub name: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    // The `/bookings/user` endpoint always returns an empty `yesParticipants`;
    // the real participant list comes through `attendees` instead.
    #[serde(default)]
    pub attendees: Vec<SessionAttendee>,
    #[serde(rename = "totalQuantityFree")]
    pub total_capacity: Option<u32>,
}

impl BookingSession {
    /// Number of active (non-deleted) participants for this booking's session.
    fn participant_count(&self) -> usize {
        self.attendees.iter().filter(|a| !a.deleted).count()
    }

    /// See [`Session::sort_key`].
    fn sort_key(&self) -> (bool, &str, &str) {
        (
            self.date.is_none(),
            self.date.as_deref().unwrap_or(""),
            self.time.as_deref().unwrap_or(""),
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct Booking {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub session: Vec<BookingSession>,
}

#[derive(Debug, Deserialize)]
pub struct SessionPlace {
    pub name: Option<String>,
    pub address: Option<String>,
    #[serde(rename = "zipCode")]
    pub zip_code: Option<String>,
    pub city: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionCoach {
    #[serde(rename = "fullName")]
    pub full_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SessionDetail {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "sessionName")]
    pub name: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
    #[serde(rename = "endTime")]
    pub end_time: Option<String>,
    pub place: Option<SessionPlace>,
    #[serde(rename = "totalQuantityFree")]
    pub total_capacity: Option<u32>,
    pub price: Option<f64>,
    pub description: Option<String>,
    pub info: Option<String>,
    pub level: Option<String>,
    pub coachs: Option<Vec<SessionCoach>>,
    #[serde(rename = "yesParticipants", default)]
    pub yes_participants: Vec<String>,
    #[serde(default)]
    pub attendees: Vec<SessionAttendee>,
}

#[derive(Debug, Deserialize)]
pub struct SessionAttendee {
    #[serde(rename = "fullName")]
    pub full_name: Option<String>,
    #[serde(default)]
    pub deleted: bool,
}

impl SessionDetail {
    pub fn display_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        lines.push(format!(
            "Session: {}",
            self.name.as_deref().unwrap_or(&self.id)
        ));

        let date = self
            .date
            .as_deref()
            .and_then(|d| d.get(..10))
            .unwrap_or("?");
        let time = self.time.as_deref().unwrap_or("?");
        let end_time = self.end_time.as_deref().unwrap_or("?");
        lines.push(format!("Date: {date}  {time} - {end_time}"));

        if let Some(place) = &self.place {
            let name = place.name.as_deref().unwrap_or("");
            let address = place.address.as_deref().unwrap_or("");
            let zip = place.zip_code.as_deref().unwrap_or("");
            let city = place.city.as_deref().unwrap_or("");
            lines.push(format!("Location: {name}, {city} ({address}, {zip})"));
        }

        let current = self.yes_participants.len();
        let capacity = self
            .total_capacity
            .map_or_else(|| "?".to_string(), |c| c.to_string());
        lines.push(format!("Participants: {current}/{capacity}"));

        if let Some(price) = self.price {
            if price == 0.0 {
                lines.push("Price: free".to_string());
            } else {
                lines.push(format!("Price: {price}€"));
            }
        }

        if let Some(level) = &self.level
            && !level.is_empty()
            && level != "allLevels"
        {
            lines.push(format!("Level: {level}"));
        }

        if let Some(coachs) = &self.coachs {
            let names: Vec<&str> = coachs
                .iter()
                .filter_map(|c| c.full_name.as_deref())
                .collect();
            if !names.is_empty() {
                lines.push(format!("Coaches: {}", names.join(", ")));
            }
        }

        if let Some(desc) = &self.description
            && !desc.is_empty()
        {
            lines.push(format!("Description: {desc}"));
        }

        if let Some(info) = &self.info {
            let trimmed = info.trim();
            if !trimmed.is_empty() {
                lines.push(format!("Info: {trimmed}"));
            }
        }

        if !self.attendees.is_empty() {
            lines.push(String::new());
            lines.push("Participants:".to_string());
            for (i, attendee) in self.attendees.iter().enumerate() {
                let name = attendee.full_name.as_deref().unwrap_or("?");
                lines.push(format!("  {}. {name}", i + 1));
            }
        }

        lines
    }
}

impl fmt::Display for Booking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.session.first();
        let date = s
            .and_then(|s| s.date.as_deref())
            .and_then(|d| d.get(..10))
            .unwrap_or("?");
        let participants = s.map_or_else(
            || "?".to_string(),
            |s| match s.total_capacity {
                Some(cap) => format!("{}/{cap} booked", s.participant_count()),
                None => format!("{} booked", s.participant_count()),
            },
        );
        write!(
            f,
            "{} | {} | {} | {}",
            s.and_then(|s| s.name.as_deref())
                .unwrap_or(&self.session_id),
            date,
            s.and_then(|s| s.time.as_deref()).unwrap_or("?"),
            participants,
        )
    }
}

#[derive(Debug)]
enum Action {
    Book,
    PreBook,
    ManageBookings,
    PreviousSessions,
    Compare,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Book => write!(f, "Book a session"),
            Action::PreBook => write!(f, "Schedule a booking for a specific time"),
            Action::ManageBookings => write!(f, "View / manage bookings"),
            Action::PreviousSessions => write!(f, "See previous sessions"),
            Action::Compare => write!(f, "Compare participants between two sessions"),
        }
    }
}

#[derive(Debug)]
pub struct SessionComparison {
    pub session_a: SessionDetail,
    pub session_b: SessionDetail,
    pub in_both: Vec<String>,
    pub only_in_a: Vec<String>,
    pub only_in_b: Vec<String>,
}

impl SessionComparison {
    pub fn display_lines(&self) -> Vec<String> {
        let label = |d: &SessionDetail| -> String {
            let date = d.date.as_deref().and_then(|s| s.get(..10)).unwrap_or("?");
            let time = d.time.as_deref().unwrap_or("?");
            format!("{} ({date} {time})", d.name.as_deref().unwrap_or(&d.id))
        };

        let mut lines = Vec::new();
        lines.push(format!("Session A: {}", label(&self.session_a)));
        lines.push(format!("Session B: {}", label(&self.session_b)));
        lines.push(String::new());

        lines.push(format!("In both ({}):", self.in_both.len()));
        if self.in_both.is_empty() {
            lines.push("  (none)".to_string());
        } else {
            for name in &self.in_both {
                lines.push(format!("  - {name}"));
            }
        }

        lines.push(String::new());
        lines.push(format!("Only in A ({}):", self.only_in_a.len()));
        if self.only_in_a.is_empty() {
            lines.push("  (none)".to_string());
        } else {
            for name in &self.only_in_a {
                lines.push(format!("  - {name}"));
            }
        }

        lines.push(String::new());
        lines.push(format!("Only in B ({}):", self.only_in_b.len()));
        if self.only_in_b.is_empty() {
            lines.push("  (none)".to_string());
        } else {
            for name in &self.only_in_b {
                lines.push(format!("  - {name}"));
            }
        }

        lines
    }
}

#[derive(Debug)]
enum BookingAction {
    ViewInfo,
    Cancel,
}

impl fmt::Display for BookingAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BookingAction::ViewInfo => write!(f, "View info"),
            BookingAction::Cancel => write!(f, "Cancel reservation"),
        }
    }
}

/// Display wrapper so accounts can be listed in the interactive multi-select.
struct AccountChoice(Account);

impl fmt::Display for AccountChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.0.label, self.0.email)
    }
}

#[derive(Debug, Error)]
pub enum BookError {
    #[error("slot not open yet: {0}")]
    SlotNotOpen(String),
    /// The endpoint answered 200 but declined the booking (e.g. the member has
    /// reached their reservation limit — `status: "noCredits"`). Retrying will
    /// not help, so this is distinct from `SlotNotOpen`.
    #[error("booking rejected ({status}): {message}")]
    Rejected { status: String, message: String },
    #[error(transparent)]
    Request(#[from] reqwest::Error),
}

/// Interpret a 200 response body from the booking endpoint.
///
/// The endpoint answers 200 for both outcomes, so success is decided by the
/// `status` field, not the HTTP code:
/// - a confirmed booking either omits `status` (returning a booking record with
///   an `_id`) or sets `status: "success"`;
/// - a soft rejection sets a non-`success` `status` and a `message` (e.g.
///   `status: "noCredits"` when the member has hit their reservation limit).
///
/// Only an explicit non-`success` status is treated as a failure, so an
/// unrecognised body without a `status` is accepted rather than dropped.
fn interpret_book_response(body: Value) -> Result<Value, BookError> {
    match body.get("status").and_then(Value::as_str) {
        Some(status) if !status.eq_ignore_ascii_case("success") => {
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(status)
                .to_string();
            Err(BookError::Rejected {
                status: status.to_string(),
                message,
            })
        }
        _ => Ok(body),
    }
}

/// Parse a human-readable time string into a local `DateTime`.
///
/// Accepted formats:
/// - `HH:MM`           - today at that time; tomorrow if the time has already passed today
/// - `YYYY-MM-DD HH:MM` - an explicit date/time
pub fn parse_when(s: &str) -> Result<DateTime<Local>> {
    let s = s.trim();

    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return dt
            .and_local_timezone(Local)
            .single()
            .ok_or_else(|| anyhow!("Ambiguous or invalid local time: {s}"));
    }

    if let Ok(t) = NaiveTime::parse_from_str(s, "%H:%M") {
        let now = Local::now();
        let today = now.date_naive().and_time(t);
        let candidate = today
            .and_local_timezone(Local)
            .single()
            .ok_or_else(|| anyhow!("Ambiguous or invalid local time: {s}"))?;

        if candidate > now {
            return Ok(candidate);
        }

        // Time already passed today — schedule for tomorrow
        let tomorrow = now
            .date_naive()
            .succ_opt()
            .ok_or_else(|| anyhow!("Date overflow"))?
            .and_time(t);
        return tomorrow
            .and_local_timezone(Local)
            .single()
            .ok_or_else(|| anyhow!("Ambiguous or invalid local time: {s}"));
    }

    Err(anyhow!(
        "Unrecognised time format '{s}'. Use HH:MM or YYYY-MM-DD HH:MM"
    ))
}

fn deserialize_array<T: serde::de::DeserializeOwned>(raw: Value) -> Result<Vec<T>> {
    match raw {
        Value::Array(_) => Ok(serde_json::from_value(raw)?),
        _ => Ok(vec![]),
    }
}

/// Build `n` left-aligned tabular columns separated by " | " for `Select`.
fn left_columns(n: usize) -> Vec<ColumnConfig> {
    (0..n)
        .map(|i| {
            if i + 1 < n {
                ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left)
            } else {
                ColumnConfig::new(ColumnAlignment::Left)
            }
        })
        .collect()
}

pub struct MonClubClient {
    config: Config,
    /// The account this client authenticates as and acts on behalf of.
    account: Account,
    http: Client,
    token: Option<String>,
    user_id: Option<String>,
}

impl fmt::Debug for MonClubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MonClubClient")
            .field("account", &self.account.label)
            .field("user_id", &self.user_id)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl MonClubClient {
    /// Build a client for the primary account (`EMAIL`/`PASSWORD`/`CUSTOM_ID`).
    pub fn new(config: Config) -> Self {
        let account = config.primary_account();
        Self::with_account(config, account)
    }

    /// Build a client that authenticates as `account`, sharing the rest of the
    /// runtime config (base url, coordinates, retry settings).
    pub fn with_account(config: Config, account: Account) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("fr"));
        headers.insert(USER_AGENT, HeaderValue::from_static("okhttp/4.12.0"));

        let http = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            config,
            account,
            http,
            token: None,
            user_id: None,
        }
    }

    /// The label of the account this client acts as.
    pub fn account_label(&self) -> &str {
        &self.account.label
    }

    fn token(&self) -> &str {
        self.token.as_deref().unwrap_or_default()
    }

    fn user_id(&self) -> &str {
        self.user_id.as_deref().unwrap_or_default()
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.config.base_url, path)
    }

    pub fn authenticate(&mut self) -> Result<()> {
        info!("Step 1 - email probe...");
        self.http
            .post(self.endpoint("/users/custom/authenticate/email/v2"))
            .query(&[("withCoachAuthentication", "true")])
            .json(&json!({"email": self.account.email}))
            .send()?
            .error_for_status()?;

        info!("Step 2 - full authentication...");
        let data: AuthResponse = self
            .http
            .post(self.endpoint("/users/custom/authenticate/v2"))
            .json(&json!({
                "credentials": {
                    "email":    self.account.email,
                    "password": self.account.password,
                },
                "customId": self.account.custom_id,
                "deviceInfo": {
                    "os":      "Android 14",
                    "model":   "Phone (2)",
                    "brand":   "Nothing",
                    "version": "3.6.0",
                },
                "coachAuthentication": false,
            }))
            .send()?
            .error_for_status()?
            .json()?;

        self.user_id = data.resolve_user_id().map(String::from);
        self.token = data.token.filter(|t| !t.is_empty());

        if self.token.is_none() {
            return Err(anyhow!("auth succeeded but response contained no token"));
        }

        info!("Authenticated. userId={:?}", self.user_id);
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let coords = match (self.config.longitude, self.config.latitude) {
            (Some(lon), Some(lat)) => json!([lon, lat]),
            _ => json!(null),
        };

        let raw: Value = self
            .http
            .post(self.endpoint("/nearfilters/favorite/myclub"))
            .header("authorization", self.token())
            .query(&[
                ("customId", self.account.custom_id.as_str()),
                ("userId", self.user_id()),
            ])
            .json(&json!({
                "filters": {
                    "tagName":     "myclub",
                    "coordinates": coords,
                    "price":       null,
                    "discipline":  null,
                    "date":        null,
                    "time":        null,
                    "level":       null,
                    "type":        null,
                    "category":    null,
                    "pinnedSlots": null,
                    "categoryId":  null,
                    "group":       null,
                },
                "coordinates": coords,
            }))
            .send()?
            .error_for_status()?
            .json()?;

        let mut sessions: Vec<Session> = deserialize_array(raw)?;
        sessions.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        Ok(sessions)
    }

    /// Fetch the user's bookings for a given temporality.
    ///
    /// `temporality` is `fromToday` for upcoming bookings, `beforeToday` for
    /// past ones.
    fn fetch_bookings(&self, temporality: &str) -> Result<Vec<Booking>> {
        let raw: Value = self
            .http
            .get(self.endpoint(&format!("/bookings/user/{}", self.user_id())))
            .header("authorization", self.token())
            .query(&[("category", "ondemand"), ("temporality", temporality)])
            .send()?
            .error_for_status()?
            .json()?;

        let mut bookings: Vec<Booking> = deserialize_array(raw)?;
        // The API returns bookings in no particular order; the pickers and the Discord listings all read better chronologically.
        bookings.sort_by(|a, b| {
            a.session
                .first()
                .map(BookingSession::sort_key)
                .cmp(&b.session.first().map(BookingSession::sort_key))
        });
        Ok(bookings)
    }

    pub fn list_bookings(&self) -> Result<Vec<Booking>> {
        self.fetch_bookings("fromToday")
    }

    pub fn list_previous_bookings(&self) -> Result<Vec<Booking>> {
        self.fetch_bookings("beforeToday")
    }

    /// Sessions available to book: the full listing minus the ones the user has
    /// already booked, so the booking picker never offers a duplicate.
    fn bookable_sessions(&self) -> Result<Vec<Session>> {
        let booked_ids: HashSet<String> = self
            .list_bookings()?
            .into_iter()
            .map(|b| b.session_id)
            .collect();

        Ok(self
            .list_sessions()?
            .into_iter()
            .filter(|s| !booked_ids.contains(&s.id))
            .collect())
    }

    fn find_target_session(&self) -> Result<Option<Session>> {
        let sessions = self.bookable_sessions()?;
        info!("{} sessions available to book", sessions.len());

        if sessions.is_empty() {
            return Ok(None);
        }

        let session = Select::new("Select a session to book:", sessions)
            .with_tabular_columns(left_columns(4))
            .prompt()?;

        Ok(Some(session))
    }

    /// Send a presence request to the shared book/cancel endpoint.
    ///
    /// `/sessions/book/licenseeFromClub` handles both booking and cancelling;
    /// the `isPresent` field on the `participant` distinguishes them. The raw
    /// response is returned so callers can apply their own status handling
    /// (booking treats `409` specially; cancellation does not).
    fn post_presence(
        &self,
        participant: &Value,
        session_id: &str,
    ) -> reqwest::Result<reqwest::blocking::Response> {
        self.http
            .post(self.endpoint("/sessions/book/licenseeFromClub"))
            .header("authorization", self.token())
            .json(&json!({
                "participant": participant,
                "sessionId":   session_id,
                "customId":    self.account.custom_id,
            }))
            .send()
    }

    pub fn book_session(&self, session: &Session) -> Result<Value, BookError> {
        info!("Booking '{}' (id={})...", session, session.id);

        let resp = self.post_presence(
            &json!({
                "userId":      self.user_id,
                "isPresent":   "yes",
                "coordinates": null,
            }),
            &session.id,
        )?;

        if resp.status() == reqwest::StatusCode::CONFLICT {
            return Err(BookError::SlotNotOpen(resp.text().unwrap_or_default()));
        }

        let body: Value = resp.error_for_status()?.json()?;

        let outcome = interpret_book_response(body);
        if let Err(BookError::Rejected { status, message }) = &outcome {
            warn!("Booking not confirmed (status={status}): {message}");
        }
        outcome
    }

    pub fn get_session(&self, session_id: &str) -> Result<SessionDetail> {
        let raw: Value = self
            .http
            .post(self.endpoint("/sessions/withuser"))
            .header("authorization", self.token())
            .json(&json!({
                "sessionId": session_id,
                "userId":    self.user_id,
            }))
            .send()?
            .error_for_status()?
            .json()?;

        let session = raw.get("session").cloned().unwrap_or(raw);

        Ok(serde_json::from_value(session)?)
    }

    pub fn cancel_booking(&self, booking: &Booking) -> Result<Value> {
        info!("Cancelling '{}' (bookingId={})...", booking, booking.id);

        Ok(self
            .post_presence(
                &json!({
                    "userId":      self.user_id,
                    "isPresent":   "no",
                    "coordinates": null,
                    "bookingId":   booking.id,
                }),
                &booking.session_id,
            )?
            .error_for_status()?
            .json()?)
    }

    /// Cancel this account's booking for `session_id`, looking up the booking
    /// id from the account's own upcoming bookings.
    ///
    /// Returns the cancelled `bookingId`, or `None` when the account has no
    /// booking for that session. Used to cancel on behalf of accounts whose
    /// `bookingId` the caller does not already hold.
    pub fn cancel_session_booking(&self, session_id: &str) -> Result<Option<String>> {
        let Some(booking) = self
            .list_bookings()?
            .into_iter()
            .find(|b| b.session_id == session_id)
        else {
            return Ok(None);
        };

        let booking_id = booking.id.clone();
        self.cancel_booking(&booking)?;
        Ok(Some(booking_id))
    }

    /// Book `session`, retrying while the server answers `409` (slot not open
    /// yet) until the configured deadline. Returns an error (rather than exiting
    /// the process) on expiry or any other failure, so multi-account callers can
    /// report one account's outcome and continue with the next. Shared by the
    /// direct-id, interactive and pre-book flows.
    fn book_with_retry(&self, session: &Session) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(self.config.retry_duration);
        let mut attempt = 0u32;

        loop {
            attempt += 1;
            info!("Attempt {attempt} - booking session '{}'...", session.id);

            match self.book_session(session) {
                Ok(_) => {
                    println!("Booking confirmed!");
                    return Ok(());
                }
                Err(BookError::SlotNotOpen(body)) => {
                    warn!("409 slot not open yet: {body}");
                    if Instant::now() >= deadline {
                        return Err(anyhow!("Booking window expired."));
                    }
                    println!(
                        "Slot not open yet. Retrying in {}s...",
                        self.config.retry_interval
                    );
                    thread::sleep(Duration::from_secs(self.config.retry_interval));
                }
                Err(e) => return Err(anyhow!("Booking failed: {e}")),
            }
        }
    }

    /// Build and authenticate a client for `account`, sharing this client's
    /// runtime config.
    fn authed_client_for(&self, account: &Account) -> Result<MonClubClient> {
        let mut client = MonClubClient::with_account(self.config.clone(), account.clone());
        client.authenticate()?;
        Ok(client)
    }

    /// Resolve which accounts a book/cancel action targets.
    ///
    /// Precedence: an explicit `--for` list (comma-separated labels) wins. With
    /// no flag, prompt to multi-select only when extra users are configured in
    /// `users.json`; when only the primary account exists, target it silently so
    /// single-user setups keep their current behaviour.
    fn resolve_targets(&self, for_arg: Option<&str>, prompt: &str) -> Result<Vec<Account>> {
        let config = &self.config;

        if let Some(spec) = for_arg {
            let mut targets: Vec<Account> = Vec::new();
            for label in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let account = config.account_for_label(label).ok_or_else(|| {
                    anyhow!(
                        "Unknown --for label '{label}' (not in users.json or the primary account)"
                    )
                })?;
                // De-duplicate: two labels (e.g. "me" and a users.json entry for
                // yourself) can point at the same MonClub identity.
                if !targets.iter().any(|a| same_identity(a, &account)) {
                    targets.push(account);
                }
            }
            if targets.is_empty() {
                return Err(anyhow!("--for was provided but empty"));
            }
            return Ok(targets);
        }

        // Distinct identities only, so the same person is never offered twice.
        // One identity (the common case) needs no prompt.
        let accounts = config.distinct_accounts();
        if accounts.len() <= 1 {
            return Ok(accounts);
        }

        // The primary "me" entry sorts first in `distinct_accounts`, so
        // preselect it as the default booking target.
        let choices: Vec<AccountChoice> = accounts.into_iter().map(AccountChoice).collect();

        let selected = MultiSelect::new(prompt, choices)
            .with_default(&[0])
            .prompt()?;

        if selected.is_empty() {
            return Err(anyhow!("No account selected."));
        }

        Ok(selected.into_iter().map(|c| c.0).collect())
    }

    /// Book `session` for each target account, reusing this (already
    /// authenticated) client when the target is its own account. Reports each
    /// account's outcome without aborting the others.
    fn book_for_all(&self, session: &Session, targets: &[Account]) {
        let multi = targets.len() > 1;

        for account in targets {
            if multi {
                println!("--- {} ---", account.label);
            }

            let result = if same_identity(account, &self.account) {
                self.book_with_retry(session)
            } else {
                match self.authed_client_for(account) {
                    Ok(client) => client.book_with_retry(session),
                    Err(e) => Err(anyhow!("authentication failed: {e}")),
                }
            };

            if let Err(e) = result {
                if multi {
                    eprintln!("{}: {e}", account.label);
                } else {
                    eprintln!("{e}");
                }
            }
        }
    }

    /// Cancel each target account's booking for `session_id`, reusing this
    /// client for its own account. Reports each account's outcome.
    fn cancel_for_all(&self, session_id: &str, targets: &[Account]) {
        let multi = targets.len() > 1;

        for account in targets {
            if multi {
                println!("--- {} ---", account.label);
            }

            let outcome = if same_identity(account, &self.account) {
                self.cancel_session_booking(session_id)
            } else {
                match self.authed_client_for(account) {
                    Ok(client) => client.cancel_session_booking(session_id),
                    Err(e) => Err(anyhow!("authentication failed: {e}")),
                }
            };

            match outcome {
                Ok(Some(id)) => println!("Cancelled booking {id} for {}.", account.label),
                Ok(None) => println!("No booking for {} on this session.", account.label),
                Err(e) => eprintln!("{}: {e}", account.label),
            }
        }
    }

    /// Resolve the session to book: the provided id, or an interactive pick
    /// (waiting for a matching session to appear, then confirming). Returns
    /// `None` when the user declines the confirmation.
    fn resolve_book_session(&self, session_id: Option<String>) -> Result<Option<Session>> {
        if let Some(id) = session_id {
            return Ok(Some(Session::from_id(id)));
        }

        let deadline = Instant::now() + Duration::from_secs(self.config.retry_duration);
        let session = loop {
            info!("Searching for target session...");
            if let Some(session) = self.find_target_session()? {
                break session;
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("Session not found after retries."));
            }
            println!(
                "No matching session yet. Retrying in {}s...",
                self.config.retry_interval
            );
            thread::sleep(Duration::from_secs(self.config.retry_interval));
        };

        let confirmed = Confirm::new(&format!("Book '{session}'?"))
            .with_default(true)
            .prompt()?;

        if !confirmed {
            println!("Booking cancelled.");
            return Ok(None);
        }

        Ok(Some(session))
    }

    pub fn run_book(&self, session_id: Option<String>, for_arg: Option<&str>) -> Result<()> {
        let targets = self.resolve_targets(for_arg, "Book for whom?")?;

        let Some(session) = self.resolve_book_session(session_id)? else {
            return Ok(());
        };

        self.book_for_all(&session, &targets);
        Ok(())
    }

    pub fn run_prebook(
        &self,
        session_id: Option<String>,
        when: Option<String>,
        for_arg: Option<&str>,
    ) -> Result<()> {
        // Step 1: choose who to book for.
        let targets = self.resolve_targets(for_arg, "Book for whom?")?;

        // Step 2: pick a session (or use the provided ID directly)
        let session = match session_id {
            Some(id) => Session::from_id(id),
            None => {
                if let Some(s) = self.find_target_session()? {
                    s
                } else {
                    let id = Text::new("No sessions found. Enter session ID manually:").prompt()?;
                    Session::from_id(id)
                }
            }
        };

        // Step 3: resolve target time
        let when_str = match when {
            Some(w) => w,
            None => Text::new("When to book? (HH:MM or YYYY-MM-DD HH:MM, local time):").prompt()?,
        };
        let target = parse_when(&when_str)?;
        let now = Local::now();

        if target <= now {
            return Err(anyhow!("Target time is already in the past"));
        }

        let wait = (target - now)
            .to_std()
            .map_err(|e| anyhow!("Duration error: {e}"))?;

        println!(
            "Will book '{}' at {}. Waiting...",
            session,
            target.format("%Y-%m-%d %H:%M:%S")
        );
        thread::sleep(wait);
        println!("Target time reached. Booking now...");

        // Step 4: book for every target once the slot opens
        self.book_for_all(&session, &targets);
        Ok(())
    }

    pub fn run_manage_bookings(&self, for_arg: Option<&str>) -> Result<()> {
        let bookings = self.list_bookings()?;

        if bookings.is_empty() {
            println!("No upcoming bookings found.");
            return Ok(());
        }

        let booking = Select::new("Select a booking:", bookings)
            .with_tabular_columns(left_columns(4))
            .prompt()?;

        let booking_action = Select::new(
            &format!("What would you like to do with '{booking}'?"),
            vec![BookingAction::ViewInfo, BookingAction::Cancel],
        )
        .prompt()?;

        match booking_action {
            BookingAction::ViewInfo => {
                let detail = self.get_session(&booking.session_id)?;
                for line in detail.display_lines() {
                    println!("{line}");
                }
            }
            BookingAction::Cancel => {
                // Choose who to cancel for (prompts only when extra users exist).
                let targets = self.resolve_targets(for_arg, "Cancel for whom?")?;

                let prompt = if targets.len() > 1 {
                    let labels: Vec<&str> = targets.iter().map(|a| a.label.as_str()).collect();
                    format!("Cancel '{booking}' for {}?", labels.join(", "))
                } else {
                    format!("Cancel '{booking}'?")
                };

                let confirmed = Confirm::new(&prompt).with_default(false).prompt()?;

                if !confirmed {
                    println!("Cancellation aborted.");
                    return Ok(());
                }

                self.cancel_for_all(&booking.session_id, &targets);
            }
        }

        Ok(())
    }

    pub fn run_previous_sessions(&self) -> Result<()> {
        let mut bookings = self.list_previous_bookings()?;
        // Past sessions read best most-recent-first, the reverse of the chronological order `list_previous_bookings` hands back.
        bookings.reverse();

        if bookings.is_empty() {
            println!("No previous sessions found.");
            return Ok(());
        }

        let booking = Select::new("Select a previous session:", bookings)
            .with_tabular_columns(left_columns(4))
            .prompt()?;

        let detail = self.get_session(&booking.session_id)?;
        for line in detail.display_lines() {
            println!("{line}");
        }

        Ok(())
    }

    pub fn compare_sessions(
        &self,
        session_id_a: &str,
        session_id_b: &str,
    ) -> Result<SessionComparison> {
        let a = self.get_session(session_id_a)?;
        let b = self.get_session(session_id_b)?;

        let names_a: HashSet<String> = a
            .attendees
            .iter()
            .filter_map(|att| att.full_name.clone())
            .collect();
        let names_b: HashSet<String> = b
            .attendees
            .iter()
            .filter_map(|att| att.full_name.clone())
            .collect();

        let mut in_both: Vec<String> = names_a.intersection(&names_b).cloned().collect();
        let mut only_in_a: Vec<String> = names_a.difference(&names_b).cloned().collect();
        let mut only_in_b: Vec<String> = names_b.difference(&names_a).cloned().collect();

        in_both.sort();
        only_in_a.sort();
        only_in_b.sort();

        Ok(SessionComparison {
            session_a: a,
            session_b: b,
            in_both,
            only_in_a,
            only_in_b,
        })
    }

    /// Build a session list with booked sessions sorted first.
    fn sessions_booked_first(&self) -> Result<Vec<Session>> {
        let mut sessions = self.list_sessions()?;
        let booked_ids: HashSet<String> = self
            .list_bookings()?
            .into_iter()
            .map(|b| b.session_id)
            .collect();
        sessions.sort_by_key(|s| !booked_ids.contains(&s.id));
        Ok(sessions)
    }

    pub fn run_compare(
        &self,
        session_id_a: Option<String>,
        session_id_b: Option<String>,
    ) -> Result<()> {
        let id_a = if let Some(id) = session_id_a {
            id
        } else {
            let sessions = self.sessions_booked_first()?;
            if sessions.is_empty() {
                return Err(anyhow!("No sessions available"));
            }
            let s = Select::new("Select session A:", sessions)
                .with_tabular_columns(left_columns(3))
                .prompt()?;
            s.id
        };

        let id_b = if let Some(id) = session_id_b {
            id
        } else {
            let sessions = self
                .sessions_booked_first()?
                .into_iter()
                .filter(|s| s.id != id_a)
                .collect::<Vec<_>>();
            if sessions.is_empty() {
                return Err(anyhow!("No other sessions available"));
            }
            let s = Select::new("Select session B:", sessions)
                .with_tabular_columns(left_columns(3))
                .prompt()?;
            s.id
        };

        let comparison = self.compare_sessions(&id_a, &id_b)?;
        for line in comparison.display_lines() {
            println!("{line}");
        }

        Ok(())
    }

    pub fn run(&mut self) -> Result<()> {
        self.authenticate()?;

        let action = Select::new(
            "What would you like to do?",
            vec![
                Action::Book,
                Action::PreBook,
                Action::ManageBookings,
                Action::PreviousSessions,
                Action::Compare,
            ],
        )
        .prompt()?;

        match action {
            Action::Book => self.run_book(None, None),
            Action::PreBook => self.run_prebook(None, None, None),
            Action::ManageBookings => self.run_manage_bookings(None),
            Action::PreviousSessions => self.run_previous_sessions(),
            Action::Compare => self.run_compare(None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_booking_record_with_id() {
        // The captured success shape: a booking record with `_id`, no `status`.
        let body = json!({"_id": "deadbeef", "isPresent": "yes"});
        assert!(interpret_book_response(body).is_ok());
    }

    #[test]
    fn accepts_status_success() {
        // The observed success shape from the live API: `status: "success"`,
        // with no `_id`. This must not be treated as a failure.
        let body = json!({"status": "success", "message": "success"});
        assert!(interpret_book_response(body).is_ok());
    }

    #[test]
    fn accepts_body_without_status() {
        // No explicit failure status -> accepted rather than dropped.
        let body = json!({"foo": "bar"});
        assert!(interpret_book_response(body).is_ok());
    }

    #[test]
    fn rejects_no_credits_despite_200() {
        // 200 OK but the member has hit their reservation limit.
        let body = json!({
            "status": "noCredits",
            "message": "L\u{2019}adh\u{e9}rent a atteint la limite de ses r\u{e9}servations autoris\u{e9}es."
        });
        match interpret_book_response(body) {
            Err(BookError::Rejected { status, message }) => {
                assert_eq!(status, "noCredits");
                assert!(message.contains("limite"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    fn account(label: &str, email: &str) -> Account {
        Account {
            label: label.to_string(),
            email: email.to_string(),
            password: "p".to_string(),
            custom_id: "club1".to_string(),
            discord_id: None,
        }
    }

    fn client_with(users: Vec<Account>) -> MonClubClient {
        let config = Config {
            email: "owner@x.com".to_string(),
            password: "p".to_string(),
            custom_id: "club1".to_string(),
            base_url: "https://example.invalid".to_string(),
            latitude: None,
            longitude: None,
            retry_duration: 1,
            retry_interval: 1,
            discord_token: None,
            discord_owner_id: Some(1),
            new_sessions_channel_id: None,
            new_sessions_poll_interval: 60,
            booking_window_hours: 144,
            watch_poll_interval: 60,
            users,
        };
        MonClubClient::new(config)
    }

    #[test]
    fn resolve_targets_for_flag_resolves_labels() {
        let client = client_with(vec![account("tom", "tom@x.com")]);
        let targets = client.resolve_targets(Some("tom"), "x").unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].label, "tom");
    }

    #[test]
    fn resolve_targets_for_flag_multiple_and_dedup() {
        let client = client_with(vec![account("tom", "tom@x.com")]);
        // "me" is the primary label; the repeated "tom" is de-duplicated.
        let targets = client.resolve_targets(Some("me, tom, tom"), "x").unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].label, "me");
        assert_eq!(targets[1].label, "tom");
    }

    #[test]
    fn resolve_targets_for_flag_unknown_label_errors() {
        let client = client_with(vec![account("tom", "tom@x.com")]);
        assert!(client.resolve_targets(Some("ghost"), "x").is_err());
    }

    #[test]
    fn resolve_targets_single_account_skips_prompt() {
        // No extra users and no flag -> primary account, without prompting.
        let client = client_with(vec![]);
        let targets = client.resolve_targets(None, "x").unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].label, "me");
    }

    #[test]
    fn resolve_targets_duplicate_self_entry_skips_prompt() {
        // A users.json entry that is really the primary account (same email)
        // collapses to one identity -> no prompt, books as "me".
        let client = client_with(vec![account("tom", "owner@x.com")]);
        let targets = client.resolve_targets(None, "x").unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].label, "me");
    }

    #[test]
    fn resolve_targets_for_flag_collapses_same_identity() {
        // "me" and a users.json alias for the same email -> a single target.
        let client = client_with(vec![account("tom", "owner@x.com")]);
        let targets = client.resolve_targets(Some("me,tom"), "x").unwrap();
        assert_eq!(targets.len(), 1);
    }

    fn session(name: &str, date: Option<&str>, time: Option<&str>) -> Session {
        Session {
            id: format!("id-{name}"),
            name: Some(name.to_string()),
            date: date.map(str::to_string),
            time: time.map(str::to_string),
            yes_participants: vec![],
            total_capacity: None,
        }
    }

    /// The ordering `list_sessions` and `fetch_bookings` apply to their results.
    fn sorted_names(mut sessions: Vec<Session>) -> Vec<String> {
        sessions.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        sessions
            .into_iter()
            .map(|s| s.name.unwrap_or_default())
            .collect()
    }

    #[test]
    fn sorts_sessions_chronologically() {
        // The API returns no particular order; the picker must show the soonest session first.
        let names = sorted_names(vec![
            session("wed", Some("2026-07-22T00:00:00.000Z"), Some("19H30")),
            session("mon", Some("2026-07-20T00:00:00.000Z"), Some("20H00")),
            session("thu", Some("2026-07-23T00:00:00.000Z"), Some("19H30")),
        ]);
        assert_eq!(names, ["mon", "wed", "thu"]);
    }

    #[test]
    fn breaks_same_day_ties_by_time() {
        // Two sessions on one day sort by start time, not by arrival order.
        let names = sorted_names(vec![
            session("evening", Some("2026-07-20T00:00:00.000Z"), Some("20H00")),
            session("morning", Some("2026-07-20T00:00:00.000Z"), Some("08H00")),
        ]);
        assert_eq!(names, ["morning", "evening"]);
    }

    #[test]
    fn sorts_dateless_sessions_last() {
        // A missing date must not sort as an empty string and jump to the top.
        let names = sorted_names(vec![
            session("undated", None, None),
            session("dated", Some("2026-07-20T00:00:00.000Z"), Some("20H00")),
        ]);
        assert_eq!(names, ["dated", "undated"]);
    }

    #[test]
    fn session_display_labels_count_as_booked() {
        // "17/24" alone reads equally well as remaining slots; the suffix pins it to slots taken.
        let mut s = session("beach", Some("2026-07-22T00:00:00.000Z"), Some("19H30"));
        s.total_capacity = Some(24);
        s.yes_participants = (0..17).map(|i| i.to_string()).collect();
        assert_eq!(s.to_string(), "beach | 2026-07-22 | 19H30 | 17/24 booked");
    }

    #[test]
    fn session_display_keeps_overbooked_count() {
        // The live API does return counts above capacity; render them as-is rather than clamping or flipping to a negative remainder.
        let mut s = session("loisir", Some("2026-07-20T00:00:00.000Z"), Some("20H00"));
        s.total_capacity = Some(36);
        s.yes_participants = (0..37).map(|i| i.to_string()).collect();
        assert!(s.to_string().ends_with("37/36 booked"));
    }

    #[test]
    fn booking_display_ignores_deleted_attendees() {
        // `/bookings/user` returns cancelled attendees with `deleted: true`; they must not inflate the booked count.
        let attendee = |deleted: bool| SessionAttendee {
            full_name: Some("someone".to_string()),
            deleted,
        };
        let booking = Booking {
            id: "b1".to_string(),
            session_id: "s1".to_string(),
            session: vec![BookingSession {
                name: Some("beach".to_string()),
                date: Some("2026-07-23T00:00:00.000Z".to_string()),
                time: Some("19H30".to_string()),
                attendees: vec![attendee(false), attendee(true), attendee(false)],
                total_capacity: Some(24),
            }],
        };
        assert_eq!(
            booking.to_string(),
            "beach | 2026-07-23 | 19H30 | 2/24 booked"
        );
    }

    #[test]
    fn booking_without_session_still_renders() {
        // A booking whose `session` array came back empty falls back to the session id rather than panicking on `first()`.
        let booking = Booking {
            id: "b1".to_string(),
            session_id: "s1".to_string(),
            session: vec![],
        };
        assert_eq!(booking.to_string(), "s1 | ? | ? | ?");
    }
}
