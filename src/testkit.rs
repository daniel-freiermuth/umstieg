//! Test fixtures: timetables written as small tables of stops and times.
//!
//! ```text
//! let mut b = Builder::new();
//! let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "09:02+5"), ("C", "10:00+4", "")]);
//! let tt = b.build("A", "C");
//! ```
//! Times are on 2026-10-24 in UTC; "09:02+5" is planned 09:02, actual 09:07.

#![expect(clippy::arithmetic_side_effects, reason = "fixture times")]

use chrono::{DateTime, TimeDelta, TimeZone, Utc};

use crate::timetable::{Call, Footpath, Station, Time, Timetable, Trip};

/// 2026-10-24 at "HH:MM" UTC.
pub fn at(hhmm: &str) -> DateTime<Utc> {
    let (h, m) = hhmm.split_once(':').unwrap_or_else(|| panic!("bad time {hhmm:?}"));
    let (h, m): (u32, u32) = (
        h.parse().unwrap_or_else(|_| panic!("bad hour in {hhmm:?}")),
        m.parse().unwrap_or_else(|_| panic!("bad minute in {hhmm:?}")),
    );
    Utc.with_ymd_and_hms(2026, 10, 24, 0, 0, 0).single().unwrap_or_else(|| panic!("fixed date"))
        + TimeDelta::minutes(i64::from(h * 60 + m))
}

/// "HH:MM" (on time) or "HH:MM+N" / "HH:MM-N" (N minutes late / early); "" is none.
pub fn time(s: &str) -> Option<Time> {
    if s.is_empty() {
        return None;
    }
    let (planned, delay) = s.find(['+', '-']).map_or((s, 0), |i| {
        let (planned, delay) = s.split_at(i);
        (planned, delay.parse::<i64>().unwrap_or_else(|_| panic!("bad delay in {s:?}")))
    });
    let planned = at(planned);
    Some(Time { planned, actual: planned + TimeDelta::minutes(delay) })
}

#[derive(Default)]
pub struct Builder {
    pub stations: Vec<Station>,
    pub trips: Vec<Trip>,
    pub footpaths: Vec<Footpath>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The station named `name`, added on first use.
    pub fn station(&mut self, name: &str) -> usize {
        if let Some(i) = self.stations.iter().position(|s| s.name == name) {
            return i;
        }
        self.stations.push(Station { name: name.to_owned(), eva: None, db_name: None, tz: None, position: None });
        self.stations.len() - 1
    }

    /// A train calling at `stops` as (station, arrival, departure); returns its index.
    /// Train numbers are the digits of the label ("ICE 774" → 774).
    pub fn trip(&mut self, label: &str, stops: &[(&str, &str, &str)]) -> usize {
        let calls = stops
            .iter()
            .map(|&(name, arrival, departure)| Call {
                station: self.station(name),
                arrival: time(arrival),
                departure: time(departure),
                platform: None,
                cancelled: false,
                from_db: true,
            })
            .collect();
        let digits: String = label.chars().filter(char::is_ascii_digit).collect();
        self.trips.push(Trip {
            label: label.to_owned(),
            numbers: digits.parse().into_iter().collect(),
            class: None,
            headsign: None,
            calls,
        });
        self.trips.len() - 1
    }

    /// Cancels `trip`'s stop at `station`.
    pub fn cancel(&mut self, trip: usize, station: &str) {
        let s = self.station(station);
        for call in self.trips[trip].calls.iter_mut().filter(|c| c.station == s) {
            call.cancelled = true;
        }
    }

    /// A walk of `minutes` from `from` to `to` (one way).
    pub fn walk(&mut self, from: &str, to: &str, minutes: i64) {
        let (from, to) = (self.station(from), self.station(to));
        self.footpaths.push(Footpath { from, to, duration: TimeDelta::minutes(minutes) });
    }

    pub fn build(mut self, origin: &str, destination: &str) -> Timetable {
        let (origin, destination) = (self.station(origin), self.station(destination));
        Timetable { stations: self.stations, trips: self.trips, footpaths: self.footpaths, origin, destination }
    }
}

/// The JSON in `<script id="{id}" type="application/json">` of a rendered page.
pub fn embedded_json(html: &str, id: &str) -> serde_json::Value {
    let open = format!("<script id=\"{id}\" type=\"application/json\">");
    let (_, rest) = html.split_once(&open).unwrap_or_else(|| panic!("no {open} in page"));
    let (json, _) = rest.split_once("</script>").unwrap_or_else(|| panic!("unclosed {open}"));
    serde_json::from_str(json.trim()).unwrap_or_else(|e| panic!("embedded JSON in {open}: {e}"))
}

/// Serves `router` on a free local port; returns its base URL ("http://127.0.0.1:port").
pub async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// A server that closes the first `drops` connections without answering
/// (transport errors), then answers every request with `status` ("200 OK") and
/// `body`. Returns its base URL and the number of connections accepted so far.
pub async fn serve_dropping(
    drops: usize,
    status: &'static str,
    body: &'static str,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            if counter.fetch_add(1, Ordering::SeqCst) < drops {
                drop(socket);
                continue;
            }
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.ok();
            });
        }
    });
    (format!("http://{addr}"), accepted)
}

/// A chain's steps as "ICE 1 A-B / walk B-C / RE 2 C-D" (labels and station names).
pub fn show(tt: &Timetable, steps: &[crate::enumerate::Step]) -> String {
    use crate::enumerate::Step;
    let name = |s: usize| tt.stations[s].name.as_str();
    steps
        .iter()
        .map(|step| match *step {
            Step::Ride { trip, board, alight } => {
                let t = &tt.trips[trip];
                format!("{} {}-{}", t.label, name(t.calls[board].station), name(t.calls[alight].station))
            }
            Step::Walk { footpath } => {
                let fp = &tt.footpaths[footpath];
                format!("walk {}-{}", name(fp.from), name(fp.to))
            }
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Riding the trip labelled `label` from station `from` to station `to`.
pub fn ride(tt: &Timetable, label: &str, from: &str, to: &str) -> crate::enumerate::Step {
    let trip = tt.trips.iter().position(|t| t.label == label).unwrap_or_else(|| panic!("no trip {label}"));
    let call = |name: &str| {
        tt.trips[trip]
            .calls
            .iter()
            .position(|c| tt.stations[c.station].name == name)
            .unwrap_or_else(|| panic!("{label} doesn't call at {name}"))
    };
    crate::enumerate::Step::Ride { trip, board: call(from), alight: call(to) }
}
