use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use inquire::{Confirm, Select};
use inquire::tabular::{ColumnAlignment, ColumnConfig};
use log::{info, warn};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

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
        let date = self.date.as_deref().and_then(|d| d.get(..10)).unwrap_or("?");
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
            s.and_then(|s| s.name.as_deref()).unwrap_or(&self.session_id),
            date,
            s.and_then(|s| s.time.as_deref()).unwrap_or("?"),
        )
    }
}

#[derive(Debug)]
enum Action {
    Book,
    Cancel,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Book => write!(f, "Book a session"),
            Action::Cancel => write!(f, "Cancel a reservation"),
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

        Self { config, http, token: None, user_id: None }
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
        let data: AuthResponse = self.http
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

    fn list_sessions(&self) -> Result<Vec<Session>> {
        let coords = match (self.config.longitude, self.config.latitude) {
            (Some(lon), Some(lat)) => json!([lon, lat]),
            _ => json!(null),
        };

        let raw: Value = self.http
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

    fn list_bookings(&self) -> Result<Vec<Booking>> {
        let raw: Value = self.http
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

    fn book_session(&self, session: &Session) -> Result<Value, BookError> {
        info!("Booking '{}' (id={})...", session, session.id);

        let resp = self.http
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

    fn cancel_booking(&self, booking: &Booking) -> Result<Value> {
        info!("Cancelling '{}' (bookingId={})...", booking, booking.id);

        Ok(self.http
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

    fn run_book(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(self.config.retry_duration);
        let mut attempt = 0u32;

        loop {
            attempt += 1;
            info!("Attempt {} - searching for target session...", attempt);

            let Some(session) = self.find_target_session()? else {
                if Instant::now() >= deadline {
                    eprintln!("Session not found after retries. Giving up.");
                    std::process::exit(1);
                }
                println!("No matching session yet. Retrying in {}s...", self.config.retry_interval);
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
                    warn!("409 slot not open yet: {}", body);
                    println!("Slot not open yet. Retrying in {}s...", self.config.retry_interval);
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

    fn run_cancel(&self) -> Result<()> {
        let bookings = self.list_bookings()?;

        if bookings.is_empty() {
            println!("No upcoming bookings found.");
            return Ok(());
        }

        let booking = Select::new("Select a reservation to cancel:", bookings)
            .with_tabular_columns(vec![
                ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                ColumnConfig::new_with_separator(" | ", ColumnAlignment::Left),
                ColumnConfig::new(ColumnAlignment::Left),
            ])
            .prompt()?;

        let confirmed = Confirm::new(&format!("Cancel '{booking}'?"))
            .with_default(false)
            .prompt()?;

        if !confirmed {
            println!("Cancellation aborted.");
            return Ok(());
        }

        self.cancel_booking(&booking)?;
        println!("Cancellation confirmed!");
        Ok(())
    }

    pub fn run(&mut self) -> Result<()> {
        self.authenticate()?;

        let action = Select::new("What would you like to do?", vec![Action::Book, Action::Cancel])
            .prompt()?;

        match action {
            Action::Book => self.run_book(),
            Action::Cancel => self.run_cancel(),
        }
    }
}
