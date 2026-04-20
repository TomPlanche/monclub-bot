use std::collections::HashSet;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, NaiveDateTime, NaiveTime};
use inquire::tabular::{ColumnAlignment, ColumnConfig};
use inquire::{Confirm, Select, Text};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tracing::{info, warn};

use crate::config::Config;

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
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let date = self
            .date
            .as_deref()
            .and_then(|d| d.get(..10))
            .unwrap_or("?");
        write!(
            f,
            "{} | {} | {}",
            self.name.as_deref().unwrap_or(&self.id),
            date,
            self.time.as_deref().unwrap_or("?"),
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct BookingSession {
    #[serde(rename = "sessionName")]
    pub name: Option<String>,
    pub date: Option<String>,
    pub time: Option<String>,
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
        write!(
            f,
            "{} | {} | {}",
            s.and_then(|s| s.name.as_deref())
                .unwrap_or(&self.session_id),
            date,
            s.and_then(|s| s.time.as_deref()).unwrap_or("?"),
        )
    }
}

#[derive(Debug)]
enum Action {
    Book,
    PreBook,
    ManageBookings,
    Compare,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Book => write!(f, "Book a session"),
            Action::PreBook => write!(f, "Schedule a booking for a specific time"),
            Action::ManageBookings => write!(f, "View / manage bookings"),
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

#[derive(Debug, Error)]
pub enum BookError {
    #[error("slot not open yet: {0}")]
    SlotNotOpen(String),
    #[error(transparent)]
    Request(#[from] reqwest::Error),
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

pub struct MonClubClient {
    config: Config,
    http: Client,
    token: Option<String>,
    user_id: Option<String>,
}

impl fmt::Debug for MonClubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MonClubClient")
            .field("user_id", &self.user_id)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl MonClubClient {
    pub fn new(config: Config) -> Self {
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
            http,
            token: None,
            user_id: None,
        }
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
            .json(&json!({"email": self.config.email}))
            .send()?
            .error_for_status()?;

        info!("Step 2 - full authentication...");
        let data: AuthResponse = self
            .http
            .post(self.endpoint("/users/custom/authenticate/v2"))
            .json(&json!({
                "credentials": {
                    "email":    self.config.email,
                    "password": self.config.password,
                },
                "customId": self.config.custom_id,
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
                ("customId", self.config.custom_id.as_str()),
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

        deserialize_array(raw)
    }

    pub fn list_bookings(&self) -> Result<Vec<Booking>> {
        let raw: Value = self
            .http
            .get(self.endpoint(&format!("/bookings/user/{}", self.user_id())))
            .header("authorization", self.token())
            .query(&[("category", "ondemand"), ("temporality", "fromToday")])
            .send()?
            .error_for_status()?
            .json()?;

        deserialize_array(raw)
    }

    fn find_target_session(&self) -> Result<Option<Session>> {
        let sessions = self.list_sessions()?;
        info!("{} sessions available", sessions.len());

        if sessions.is_empty() {
            return Ok(None);
        }

        let session = Select::new("Select a session to book:", sessions)
            .with_tabular_columns(vec![
                ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                ColumnConfig::new(ColumnAlignment::Left),
            ])
            .prompt()?;

        Ok(Some(session))
    }

    pub fn book_session(&self, session: &Session) -> Result<Value, BookError> {
        info!("Booking '{}' (id={})...", session, session.id);

        let resp = self
            .http
            .post(self.endpoint("/sessions/book/licenseeFromClub"))
            .header("authorization", self.token())
            .json(&json!({
                "participant": {
                    "userId":      self.user_id,
                    "isPresent":   "yes",
                    "coordinates": null,
                },
                "sessionId": session.id,
                "customId":  self.config.custom_id,
            }))
            .send()?;

        if resp.status() == reqwest::StatusCode::CONFLICT {
            return Err(BookError::SlotNotOpen(resp.text().unwrap_or_default()));
        }

        Ok(resp.error_for_status()?.json()?)
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
            .http
            .post(self.endpoint("/sessions/book/licenseeFromClub"))
            .header("authorization", self.token())
            .json(&json!({
                "participant": {
                    "userId":      self.user_id,
                    "isPresent":   "no",
                    "coordinates": null,
                    "bookingId":   booking.id,
                },
                "sessionId": booking.session_id,
                "customId":  self.config.custom_id,
            }))
            .send()?
            .error_for_status()?
            .json()?)
    }

    pub fn run_book(&self, session_id: Option<String>) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(self.config.retry_duration);
        let mut attempt = 0u32;

        // When an ID is provided directly, skip the interactive picker entirely.
        if let Some(id) = session_id {
            let session = Session {
                id,
                name: None,
                date: None,
                time: None,
            };
            loop {
                attempt += 1;
                info!("Attempt {attempt} - booking session '{}'...", session.id);

                match self.book_session(&session) {
                    Ok(_) => {
                        println!("Booking confirmed!");
                        return Ok(());
                    }
                    Err(BookError::SlotNotOpen(body)) => {
                        warn!("409 slot not open yet: {body}");
                        if Instant::now() >= deadline {
                            eprintln!("Booking window expired.");
                            std::process::exit(1);
                        }
                        println!(
                            "Slot not open yet. Retrying in {}s...",
                            self.config.retry_interval
                        );
                        thread::sleep(Duration::from_secs(self.config.retry_interval));
                    }
                    Err(e) => {
                        eprintln!("Booking failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }

        loop {
            attempt += 1;
            info!("Attempt {attempt} - searching for target session...");

            let Some(session) = self.find_target_session()? else {
                if Instant::now() >= deadline {
                    eprintln!("Session not found after retries. Giving up.");
                    std::process::exit(1);
                }
                println!(
                    "No matching session yet. Retrying in {}s...",
                    self.config.retry_interval
                );
                thread::sleep(Duration::from_secs(self.config.retry_interval));
                continue;
            };

            let confirmed = Confirm::new(&format!("Book '{session}'?"))
                .with_default(true)
                .prompt()?;

            if !confirmed {
                println!("Booking cancelled.");
                return Ok(());
            }

            match self.book_session(&session) {
                Ok(_) => {
                    println!("Booking confirmed!");
                    return Ok(());
                }
                Err(BookError::SlotNotOpen(body)) => {
                    warn!("409 slot not open yet: {body}");
                    println!(
                        "Slot not open yet. Retrying in {}s...",
                        self.config.retry_interval
                    );
                    if Instant::now() >= deadline {
                        eprintln!("Booking window expired.");
                        std::process::exit(1);
                    }
                    thread::sleep(Duration::from_secs(self.config.retry_interval));
                }
                Err(e) => {
                    eprintln!("Booking failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    pub fn run_prebook(&self, session_id: Option<String>, when: Option<String>) -> Result<()> {
        // Step 1: pick a session (or use the provided ID directly)
        let session = match session_id {
            Some(id) => Session {
                id,
                name: None,
                date: None,
                time: None,
            },
            None => if let Some(s) = self.find_target_session()? { s } else {
                let id = Text::new("No sessions found. Enter session ID manually:").prompt()?;
                Session {
                    id,
                    name: None,
                    date: None,
                    time: None,
                }
            },
        };

        // Step 2: resolve target time
        let when_str = match when {
            Some(w) => w,
            None => Text::new("When to book? (HH:MM or YYYY-MM-DD HH:MM, local time):")
                .prompt()?,
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

        // Step 3: booking retry loop
        let deadline = Instant::now() + Duration::from_secs(self.config.retry_duration);
        let mut attempt = 0u32;

        loop {
            attempt += 1;
            info!("Pre-book attempt {attempt}...");

            match self.book_session(&session) {
                Ok(_) => {
                    println!("Booking confirmed!");
                    return Ok(());
                }
                Err(BookError::SlotNotOpen(body)) => {
                    warn!("409 slot not open yet: {body}");
                    if Instant::now() >= deadline {
                        eprintln!("Booking window expired.");
                        std::process::exit(1);
                    }
                    println!(
                        "Slot not open yet. Retrying in {}s...",
                        self.config.retry_interval
                    );
                    thread::sleep(Duration::from_secs(self.config.retry_interval));
                }
                Err(e) => {
                    eprintln!("Booking failed: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    pub fn run_manage_bookings(&self) -> Result<()> {
        let bookings = self.list_bookings()?;

        if bookings.is_empty() {
            println!("No upcoming bookings found.");
            return Ok(());
        }

        let booking = Select::new("Select a booking:", bookings)
            .with_tabular_columns(vec![
                ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                ColumnConfig::new(ColumnAlignment::Left),
            ])
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
                let confirmed = Confirm::new(&format!("Cancel '{booking}'?"))
                    .with_default(false)
                    .prompt()?;

                if !confirmed {
                    println!("Cancellation aborted.");
                    return Ok(());
                }

                self.cancel_booking(&booking)?;
                println!("Cancellation confirmed!");
            }
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
        let id_a = match session_id_a {
            Some(id) => id,
            None => {
                let sessions = self.sessions_booked_first()?;
                if sessions.is_empty() {
                    return Err(anyhow!("No sessions available"));
                }
                let s = Select::new("Select session A:", sessions)
                    .with_tabular_columns(vec![
                        ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                        ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                        ColumnConfig::new(ColumnAlignment::Left),
                    ])
                    .prompt()?;
                s.id
            }
        };

        let id_b = match session_id_b {
            Some(id) => id,
            None => {
                let sessions = self
                    .sessions_booked_first()?
                    .into_iter()
                    .filter(|s| s.id != id_a)
                    .collect::<Vec<_>>();
                if sessions.is_empty() {
                    return Err(anyhow!("No other sessions available"));
                }
                let s = Select::new("Select session B:", sessions)
                    .with_tabular_columns(vec![
                        ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                        ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                        ColumnConfig::new(ColumnAlignment::Left),
                    ])
                    .prompt()?;
                s.id
            }
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
                Action::Compare,
            ],
        )
        .prompt()?;

        match action {
            Action::Book => self.run_book(None),
            Action::PreBook => self.run_prebook(None, None),
            Action::ManageBookings => self.run_manage_bookings(),
            Action::Compare => self.run_compare(None, None),
        }
    }
}
