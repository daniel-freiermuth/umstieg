use std::collections::{HashMap, HashSet};

use chrono::TimeDelta;
use chrono_tz::Europe::Berlin;
use chrono_tz::UTC;
use serde_json::Value;

use super::{Builder, Metric, Plot, heights, render};
use crate::enumerate::{self, Chain, Limits, Network, Step};
use crate::fallback::Fallbacks;
use crate::testkit::{self, at, embedded_json};
use crate::timetable::{Timetable, meters};

const MIN: TimeDelta = TimeDelta::minutes(5);

fn ride(trip: usize, board: usize, alight: usize) -> Step {
    Step::Ride { trip, board, alight }
}

fn station(tt: &Timetable, name: &str) -> usize {
    tt.stations.iter().position(|s| s.name == name).unwrap()
}

/// A piece point: (unix minutes, station).
fn pt(tt: &Timetable, hhmm: &str, name: &str) -> (i64, usize) {
    (at(hhmm).timestamp().div_euclid(60), station(tt, name))
}

fn builder(tt: &Timetable) -> Builder<'_> {
    Builder {
        tt,
        min_transfer: MIN,
        zone: UTC,
        used: HashSet::from([tt.origin, tt.destination]),
        pieces: Vec::new(),
        index: HashMap::new(),
    }
}

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-6)
}

/// A → B → C with a slow and a fast train on B → C, and D off the network.
fn network() -> Timetable {
    let mut b = testkit::Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    b.trip("RE 2", &[("B", "", "10:00"), ("C", "10:30", "")]);
    b.trip("ICE 3", &[("B", "", "12:00"), ("C", "12:20", "")]);
    b.trip("Bus 4", &[("A", "", "08:00"), ("D", "08:30", "")]);
    b.build("A", "C")
}

#[test]
fn time_heights_are_remaining_minutes_with_and_without_waits() {
    let tt = network();
    let net = Network::new(&tt).unwrap();
    let (a, b, c, d) = (station(&tt, "A"), station(&tt, "B"), station(&tt, "C"), station(&tt, "D"));

    let remaining = heights(&net, Metric::RemainingTime).unwrap();
    // Fastest ride per stretch, waits ignored: 60 + 20. D only has the bus from
    // A, ridden back for its height: 30 + 80.
    assert!(close(remaining[a], 80.0) && close(remaining[b], 20.0) && close(remaining[c], 0.0), "{remaining:?}");
    assert!(close(remaining[d], 110.0), "{remaining:?}");

    let real = heights(&net, Metric::FastestReal).unwrap();
    // 08:00 → 10:30 including the hour at B; from B the 20-minute ICE.
    assert!(close(real[a], 150.0) && close(real[b], 20.0) && close(real[c], 0.0), "{real:?}");
    assert_eq!(real[d], None);
}

#[test]
fn geographic_heights_follow_their_geometry() {
    let mut b = testkit::Builder::new();
    for name in ["Start", "Mid", "Side", "Beyond", "Goal", "Nowhere"] {
        b.station(name);
    }
    let mut tt = b.build("Start", "Goal");
    let positions = [Some((0.0, 0.0)), Some((0.0, 0.5)), Some((0.5, 0.5)), Some((0.0, 1.5)), Some((0.0, 1.0)), None];
    for (s, p) in tt.stations.iter_mut().zip(positions) {
        s.position = p;
    }
    let net = Network::new(&tt).unwrap();
    let d = meters((0.0, 0.0), (0.0, 1.0)) / 1000.0;
    // (station, hyperbolic, axis, ratio)
    let cases = [
        ("Start", d, d, 100.0),
        ("Mid", d / 2.0, d / 2.0, 50.0),
        // Equally far from both ends: halfway in every metric.
        ("Side", d / 2.0, d / 2.0, 50.0),
        // Past the goal: hyperbolic stays 0, the axis goes negative.
        ("Beyond", 0.0, -d / 2.0, 25.0),
        ("Goal", 0.0, 0.0, 0.0),
    ];
    let hyperbolic = heights(&net, Metric::Hyperbolic).unwrap();
    let axis = heights(&net, Metric::Axis).unwrap();
    let ratio = heights(&net, Metric::Ratio).unwrap();
    for (name, h, x, r) in cases {
        let s = station(&tt, name);
        assert!(close(hyperbolic[s], h), "{name} hyperbolic {:?} != {h}", hyperbolic[s]);
        assert!((axis[s].unwrap() - x).abs() < 1e-3, "{name} axis {:?} != {x}", axis[s]);
        assert!(close(ratio[s], r), "{name} ratio {:?} != {r}", ratio[s]);
    }
    let nowhere = station(&tt, "Nowhere");
    assert_eq!((hyperbolic[nowhere], axis[nowhere], ratio[nowhere]), (None, None, None));
}

#[test]
fn geographic_heights_need_both_ends_located() {
    let mut tt = network();
    tt.stations[tt.destination].position = Some((0.0, 1.0));
    let net = Network::new(&tt).unwrap();
    for metric in [Metric::Hyperbolic, Metric::Axis, Metric::Ratio] {
        let err = heights(&net, metric).unwrap_err().to_string();
        assert!(err.contains(&format!("{metric:?}")) && err.contains("coordinates"), "{err}");
    }
    assert!(heights(&net, Metric::RemainingTime).is_ok_and(|h| h.len() == tt.stations.len()));
}

#[test]
fn a_ride_has_a_point_per_arrival_and_departure_between_its_ends() {
    let mut b = testkit::Builder::new();
    let ice = b
        .trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "09:02+5"), ("C", "10:00+4", "10:05"), ("D", "11:00", "")]);
    b.trip("RE 9", &[("B", "", "09:30"), ("D", "10:00", "")]);
    let mut tt = b.build("A", "D");
    tt.trips[ice].headsign = Some("D".to_owned());
    let mut plot = builder(&tt);
    let pieces = plot.pieces(&Chain { steps: vec![ride(ice, 0, 2)] }, None, None).unwrap();
    assert_eq!(pieces, [0]);
    let piece = &plot.pieces[0];
    assert_eq!((piece.kind, piece.trip, piece.warn), ("ride", Some(ice), false));
    // Actual times; nothing beyond the alighting stop.
    assert_eq!(piece.pts, [pt(&tt, "08:00", "A"), pt(&tt, "09:00", "B"), pt(&tt, "09:07", "B"), pt(&tt, "10:04", "C")]);
    assert_eq!(piece.info, "ICE 1 → D · 08:00 UTC A → 10:04 UTC (+4) C");
    // Rows for where one boards and alights, not for stops passed.
    let used: HashSet<usize> = ["A", "C", "D"].iter().map(|n| station(&tt, n)).collect();
    assert_eq!(plot.used, used);
}

#[test]
fn waits_carry_the_buffer_and_warn_below_the_minimum_transfer() {
    // (departure at B after arriving 09:00, info, warn, drawn from, drawn to)
    let cases = [
        ("09:12", "12m at B", false, "09:00", "09:12"),
        ("09:05", "5m at B", false, "09:00", "09:05"),
        ("09:03", "3m⚠ at B", true, "09:00", "09:03"),
        // Missed: drawn forward from the departure to the arrival.
        ("08:58", "missed 2m✗ at B", true, "08:58", "09:00"),
    ];
    for (departure, info, warn, from, to) in cases {
        let mut b = testkit::Builder::new();
        let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
        let re = b.trip("RE 2", &[("B", "", departure), ("C", "10:00", "")]);
        let tt = b.build("A", "C");
        let mut plot = builder(&tt);
        let pieces = plot.pieces(&Chain { steps: vec![ride(ice, 0, 1), ride(re, 0, 1)] }, None, None).unwrap();
        let kinds: Vec<&str> = pieces.iter().map(|&i| plot.pieces[i].kind).collect();
        assert_eq!(kinds, ["ride", "wait", "ride"], "{departure}");
        let wait = &plot.pieces[pieces[1]];
        assert_eq!((wait.info.as_str(), wait.warn, wait.trip), (info, warn, None), "{departure}");
        assert_eq!(wait.pts, [pt(&tt, from, "B"), pt(&tt, to, "B")], "{departure}");
    }
}

#[test]
fn a_fallback_starts_waiting_at_the_missed_departure_without_warning() {
    let mut b = testkit::Builder::new();
    let x = b.trip("RE 5", &[("B", "", "09:10"), ("C", "10:00", "")]);
    let y = b.trip("S 6", &[("C", "", "10:02"), ("D", "10:30", "")]);
    let tt = b.build("A", "D");
    let mut plot = builder(&tt);
    let chain = Chain { steps: vec![ride(x, 0, 1), ride(y, 0, 1)] };
    let pieces = plot.pieces(&chain, None, Some((station(&tt, "B"), at("09:08")))).unwrap();
    let kinds: Vec<&str> = pieces.iter().map(|&i| plot.pieces[i].kind).collect();
    assert_eq!(kinds, ["wait", "ride", "wait", "ride"]);
    let start = &plot.pieces[pieces[0]];
    // Two minutes, but waiting after a missed train is no change that could be too short.
    assert_eq!((start.info.as_str(), start.warn), ("2m at B after the missed train", false));
    assert_eq!(start.pts, [pt(&tt, "09:08", "B"), pt(&tt, "09:10", "B")]);
    // The fallback's own changes are flagged as usual.
    let change = &plot.pieces[pieces[2]];
    assert_eq!((change.info.as_str(), change.warn), ("2m⚠ at C", true));
}

#[test]
fn walks_are_drawn_from_the_arrival_and_the_wait_after_them_counts_from_the_walk_end() {
    let mut b = testkit::Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    let s = b.trip("S 2", &[("B2", "", "09:10"), ("C", "09:30", "")]);
    b.walk("B", "B2", 4);
    let tt = b.build("A", "C");
    let mut plot = builder(&tt);
    let pieces = plot
        .pieces(&Chain { steps: vec![ride(ice, 0, 1), Step::Walk { footpath: 0 }, ride(s, 0, 1)] }, None, None)
        .unwrap();
    let got: Vec<(&str, &str, bool)> =
        pieces.iter().map(|&i| (plot.pieces[i].kind, plot.pieces[i].info.as_str(), plot.pieces[i].warn)).collect();
    assert_eq!(got[1..3], [("walk", "walk 4m to B2", false), ("wait", "6m at B2", false)]);
    assert_eq!(plot.pieces[pieces[1]].pts, [pt(&tt, "09:00", "B"), pt(&tt, "09:04", "B2")]);
    assert_eq!(plot.pieces[pieces[2]].pts, [pt(&tt, "09:04", "B2"), pt(&tt, "09:10", "B2")]);
    assert!(plot.used.contains(&station(&tt, "B2")));
}

#[test]
fn until_cuts_before_the_rejoining_ride_but_keeps_the_wait_for_it() {
    let mut b = testkit::Builder::new();
    let s1 = b.trip("S1", &[("B", "", "12:42"), ("Altona", "12:54", "")]);
    let rj = b.trip("RJ 384", &[("Altona", "", "13:08"), ("Flensburg", "15:00", "")]);
    let tt = b.build("A", "K");
    let chain = Chain { steps: vec![ride(s1, 0, 1), ride(rj, 0, 1)] };
    let start = Some((station(&tt, "B"), at("12:30")));

    let mut plot = builder(&tt);
    let pieces = plot.pieces(&chain, Some(1), start).unwrap();
    let got: Vec<(&str, &str)> = pieces.iter().map(|&i| (plot.pieces[i].kind, plot.pieces[i].info.as_str())).collect();
    assert_eq!(
        got,
        [
            ("wait", "12m at B after the missed train"),
            ("ride", "S1 · 12:42 UTC B → 12:54 UTC Altona"),
            ("wait", "14m at Altona")
        ]
    );
    assert!(!plot.used.contains(&station(&tt, "Flensburg")));

    let mut plot = builder(&tt);
    let pieces = plot.pieces(&chain, Some(0), start).unwrap();
    assert_eq!(pieces.len(), 1);
    assert_eq!(plot.pieces[pieces[0]].kind, "wait");
}

#[test]
fn identical_stretches_are_stored_once() {
    let mut b = testkit::Builder::new();
    let ice = b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "09:02"), ("C", "10:00", "")]);
    let re = b.trip("RE 2", &[("B", "", "09:10"), ("C", "10:10", "")]);
    let s = b.trip("S 3", &[("B", "", "09:20"), ("C", "10:20", "")]);
    let tt = b.build("A", "C");
    let mut plot = builder(&tt);
    let first = plot.pieces(&Chain { steps: vec![ride(ice, 0, 1), ride(re, 0, 1)] }, None, None).unwrap();
    let second = plot.pieces(&Chain { steps: vec![ride(ice, 0, 1), ride(s, 0, 1)] }, None, None).unwrap();
    let again = plot.pieces(&Chain { steps: vec![ride(ice, 0, 1), ride(re, 0, 1)] }, None, None).unwrap();
    assert_eq!(first[0], second[0], "same train, same stretch");
    assert_ne!(first[1], second[1], "waits of different length");
    assert_eq!(first, again);
    // The whole run of the train is another stretch than its first part.
    let whole = plot.pieces(&Chain { steps: vec![ride(ice, 0, 2)] }, None, None).unwrap();
    assert!(!first.contains(&whole[0]));
    assert_eq!(plot.pieces.len(), 6);
    // A fallback's start wait over the same span as a change is another piece.
    let start =
        plot.pieces(&Chain { steps: vec![ride(re, 0, 1)] }, None, Some((station(&tt, "B"), at("09:00")))).unwrap();
    assert_ne!(start[0], first[1]);
    assert_eq!(plot.pieces[start[0]].pts, plot.pieces[first[1]].pts);
}

fn located() -> Timetable {
    let mut b = testkit::Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00"), ("B", "09:00", "")]);
    b.trip("RE 2", &[("B", "", "09:10"), ("C", "10:00", "")]);
    b.trip("RE 4", &[("B", "", "09:40"), ("C", "10:30", "")]);
    b.trip("Bus 3", &[("A", "", "08:30"), ("X", "09:30", "09:31"), ("C", "11:00", "")]);
    let mut tt = b.build("A", "C");
    for (s, p) in tt.stations.iter_mut().zip([(49.48, 8.47), (50.1, 8.66), (53.55, 10.0), (51.0, 9.0)]) {
        s.position = Some(p);
    }
    tt
}

fn chains(net: &Network<'_>) -> Vec<enumerate::Group> {
    let limits = Limits {
        origin: net.tt.origin,
        depart_from: at("07:00"),
        depart_until: at("09:00"),
        max_changes: 3,
        extra_time: TimeDelta::hours(5),
        min_transfer: MIN,
        missed_by: TimeDelta::zero(),
        keep_detours: false,
        forward_only: false,
        max_nodes: 1_000_000,
    };
    enumerate::group(net.tt, enumerate::enumerate(net, &limits).unwrap().chains).unwrap()
}

fn render_data(tt: &Timetable, metric: Metric) -> anyhow::Result<Value> {
    let net = Network::new(tt)?;
    let groups = chains(&net);
    // Shown in Berlin, whatever the stations' zones (here none).
    let mut fallbacks = Fallbacks::new(&net, MIN, Berlin);
    let plot = Plot {
        title: "A → C".to_owned(),
        subtitle: "sub".to_owned(),
        groups: &groups,
        metric,
        min_transfer: MIN,
        zone: Berlin,
        back: Some("/?from=A&to=C".to_owned()),
        details_url: None,
        progress: &|_, _| {},
    };
    Ok(embedded_json(&render(&net, &mut fallbacks, &plot)?.html, "data"))
}

fn idx(v: &Value) -> usize {
    usize::try_from(v.as_u64().unwrap()).unwrap()
}

fn indexes(v: &Value) -> Vec<usize> {
    v.as_array().unwrap().iter().map(idx).collect()
}

#[test]
fn rendered_data_is_consistent() {
    let tt = located();
    let data = render_data(&tt, Metric::Axis).unwrap();
    assert_eq!(data["back"], "/?from=A&to=C");
    assert_eq!(data["title"], "A → C");
    assert_eq!(data["tz"], "Europe/Berlin", "the zone of the plot's time axis");

    let ids: Vec<&str> = data["metrics"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["remaining-time", "fastest-real", "hyperbolic", "axis", "ratio"]);
    let units: Vec<&str> = data["metrics"].as_array().unwrap().iter().map(|m| m["unit"].as_str().unwrap()).collect();
    assert_eq!(units, ["min", "min", "km", "km", "%"]);
    let axis = &data["metrics"][3]["description"];
    assert_eq!(axis, "distance left along the start–goal axis", "the page's legend explains the metric");
    assert_eq!(data["metrics"][idx(&data["metric"])]["id"], "axis");
    for m in data["metrics"].as_array().unwrap() {
        assert_eq!(m["heights"].as_array().unwrap().len(), tt.stations.len());
        assert_eq!(m["heights"][tt.destination], 0.0, "{}", m["id"]);
    }

    let names = data["names"].as_array().unwrap();
    assert_eq!(names.len(), tt.stations.len());
    let stations = indexes(&data["stations"]);
    assert!(stations.is_sorted());
    // X is only passed through.
    let want: Vec<usize> = ["A", "B", "C"].iter().map(|n| station(&tt, n)).collect();
    assert_eq!(stations, want);

    let pieces = data["pieces"].as_array().unwrap();
    for p in pieces {
        for point in p["pts"].as_array().unwrap() {
            assert!(idx(&point[1]) < names.len());
        }
        if !p["trip"].is_null() {
            assert!(idx(&p["trip"]) < data["trips"].as_array().unwrap().len());
        }
    }
    let groups = data["groups"].as_array().unwrap();
    let chains = data["chains"].as_array().unwrap();
    let counted: usize = groups.iter().map(|g| idx(&g["count"])).sum();
    assert_eq!(counted, chains.len());
    assert_eq!(chains.len(), 3, "ICE with RE 2 or RE 4, and the bus");
    let fallbacks = data["fallbacks"].as_array().unwrap();
    let details = data["details"].as_array().unwrap();
    assert_eq!(details.len(), chains.len(), "one per connection, in the page itself");
    for (c, d) in chains.iter().zip(details) {
        assert!(idx(&c["group"]) < groups.len());
        assert!(indexes(&c["pieces"]).iter().all(|&i| i < pieces.len()));
        for r in d["fallbacks"].as_array().unwrap() {
            assert!(idx(&r[0]) < fallbacks.len());
        }
    }
    for f in fallbacks {
        assert!(indexes(&f["pieces"]).iter().all(|&i| i < pieces.len()));
    }

    // Missing RE 2 at B: RE 4 at 09:40, half an hour late, waited for from 09:10.
    let (chain, change) =
        chains.iter().zip(details).find(|(_, d)| d["route"].as_str().unwrap().contains("RE 2")).unwrap();
    assert_eq!(chain["header"], "10:00 CEST → 12:00 CEST  2h00  1×  min 10m");
    let refs = change["fallbacks"].as_array().unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0][1], "12:30 CEST (+30m)");
    let fallback = &fallbacks[idx(&refs[0][0])];
    assert_eq!(fallback["at"], "B");
    let first = &pieces[indexes(&fallback["pieces"])[0]];
    assert_eq!(first["kind"], "wait");
    assert_eq!(first["pts"][0], serde_json::json!([at("09:10").timestamp().div_euclid(60), station(&tt, "B")]));
    assert_eq!(first["warn"], false);
}

#[test]
fn only_the_requested_metric_must_be_computable() {
    let mut tt = located();
    tt.stations[tt.origin].position = None;
    let err = render_data(&tt, Metric::Ratio).unwrap_err().to_string();
    assert!(err.contains("Ratio"), "{err}");
    let data = render_data(&tt, Metric::FastestReal).unwrap();
    let ids: Vec<&str> = data["metrics"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["remaining-time", "fastest-real"]);
    assert_eq!(data["metric"], 1);
}

#[test]
fn time_heights_are_rounded_to_hundredths() {
    let mut b = testkit::Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00"), ("C", "08:10", "")]);
    let mut tt = b.build("A", "C");
    // 10 min 20 s.
    let arrival = tt.trips[0].calls[1].arrival.as_mut().unwrap();
    arrival.actual += TimeDelta::seconds(20);
    arrival.planned += TimeDelta::seconds(20);
    let data = render_data(&tt, Metric::FastestReal).unwrap();
    assert_eq!(data["metrics"][1]["heights"][tt.origin], 10.33);
    // Remaining ride time counts whole minutes, as the plot draws them.
    assert_eq!(data["metrics"][0]["heights"][tt.origin], 10.0);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(300))]

    /// No drawn ride or walk changes height by more than it lasts (at most
    /// 45°), whatever delays, cancelled stops, missing times or walks: in the
    /// data the page draws, connections and fallbacks alike. Every drawn
    /// station has a height.
    #[test]
    fn remaining_time_never_rises_or_falls_faster_than_the_clock(spec in testkit::spec(true, true)) {
        let tt = testkit::timetable(&spec);
        let data = render_data(&tt, Metric::RemainingTime).unwrap();
        let heights = &data["metrics"][0]["heights"];
        for piece in data["pieces"].as_array().unwrap().iter().filter(|p| p["kind"] != "wait") {
            for pair in piece["pts"].as_array().unwrap().windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                let height = |p: &Value| heights[idx(&p[1])].as_f64().unwrap_or_else(|| panic!("no height for {p}"));
                let minutes = b[0].as_i64().unwrap().saturating_sub(a[0].as_i64().unwrap()).max(0);
                let minutes = f64::from(i32::try_from(minutes).unwrap());
                let rise = (height(b) - height(a)).abs();
                proptest::prop_assert!(rise <= minutes, "{} {a} → {b}: {rise} in {minutes} min", piece["info"]);
            }
        }
    }
}

#[test]
fn drawing_reports_the_connections_done() {
    let tt = located();
    let net = Network::new(&tt).unwrap();
    let groups = chains(&net);
    let mut fallbacks = Fallbacks::new(&net, MIN, Berlin);
    let reports = std::sync::Mutex::new(Vec::new());
    let progress = |done, total| reports.lock().unwrap().push((done, total));
    let plot = Plot {
        title: String::new(),
        subtitle: String::new(),
        groups: &groups,
        metric: Metric::Axis,
        min_transfer: MIN,
        back: None,
        details_url: Some("/job/1/chains".to_owned()),
        zone: Berlin,
        progress: &progress,
    };
    let rendered = render(&net, &mut fallbacks, &plot).unwrap();
    // Tests report before every connection.
    assert_eq!(*reports.lock().unwrap(), [(0, 3), (1, 3), (2, 3)]);
    assert_eq!(embedded_json(&rendered.html, "data")["details"], Value::Null, "not embedded");
    assert_eq!(rendered.details.len(), 3);
}
