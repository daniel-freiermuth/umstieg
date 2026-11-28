use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::testkit::{Builder, at};

/// "HH:MM" as Unix seconds; "" none.
fn s(hhmm: &str) -> Option<i64> {
    (!hhmm.is_empty()).then(|| at(hhmm).as_second())
}

fn json_of(item: &Item) -> String {
    serde_json::to_string(item).unwrap()
}

/// A MOTIS place: name, position, (scheduled, live) arrival and departure in "HH:MM" ("" none).
fn motis_place(
    name: &str,
    at_: Option<(f64, f64)>,
    arrival: (&str, &str),
    departure: (&str, &str),
) -> serde_json::Value {
    let t = |hhmm: &str| (!hhmm.is_empty()).then(|| at(hhmm).to_string());
    json!({
        "name": name,
        "lat": at_.map(|p| p.0),
        "lon": at_.map(|p| p.1),
        "scheduledArrival": t(arrival.0),
        "arrival": t(arrival.1),
        "scheduledDeparture": t(departure.0),
        "departure": t(departure.1),
    })
}

fn leg(trip_id: Option<&str>, from: serde_json::Value, stops: Vec<serde_json::Value>, to: serde_json::Value) -> Leg {
    let mut leg = json!({
        "mode": "REGIONAL_RAIL",
        "startTime": at("08:00").to_string(),
        "endTime": at("09:00").to_string(),
        "tripId": trip_id,
    });
    leg["from"] = from;
    leg["to"] = to;
    leg["intermediateStops"] = serde_json::Value::Array(stops);
    serde_json::from_value(leg).unwrap()
}

#[test]
fn items_serialize_as_the_page_reads_them() {
    let place = |name: &str| Place { name: name.to_owned(), at: (49.47935, 8.46895) };
    let ends = Item::Ends { from: place("A"), to: place("B"), depart_from: 10, depart_until: 20 };
    assert_eq!(
        json_of(&ends),
        r#"{"kind":"ends","from":{"name":"A","at":[49.47935,8.46895]},"to":{"name":"B","at":[49.47935,8.46895]},"depart_from":10,"depart_until":20}"#
    );
    let asked =
        Item::Asked { place: place("A"), span: Some((1, 2)), seen: vec![Seen("db:x".to_owned(), None, Some(3))] };
    assert_eq!(
        json_of(&asked),
        r#"{"kind":"asked","place":{"name":"A","at":[49.47935,8.46895]},"span":[1,2],"seen":[["db:x",null,3]]}"#
    );
    let unspanned = Item::Asked { place: place("A"), span: None, seen: Vec::new() };
    assert_eq!(
        json_of(&unspanned),
        r#"{"kind":"asked","place":{"name":"A","at":[49.47935,8.46895]},"span":null,"seen":[]}"#
    );
    let train = Item::Train { id: "t1".to_owned(), stops: vec![Stop(1.5, 2.25, None, Some(7))] };
    assert_eq!(json_of(&train), r#"{"kind":"train","id":"t1","stops":[[1.5,2.25,null,7]]}"#);
    let timetable = Item::Timetable {
        stations: vec![
            Station { name: "A".to_owned(), at: Some((1.5, 2.5)), height: Some(512) },
            Station { name: "B".to_owned(), at: None, height: None },
        ],
        trains: vec![vec![Call(0, None, Some(5)), Call(1, Some(9), None)]],
    };
    assert_eq!(
        json_of(&timetable),
        r#"{"kind":"timetable","stations":[{"name":"A","at":[1.5,2.5],"height":512},{"name":"B","at":null,"height":null}],"trains":[[[0,null,5],[1,9,null]]]}"#
    );
    assert_eq!(json_of(&Item::AskedTrain { trip: 5 }), r#"{"kind":"asked_train","trip":5}"#);
}

#[test]
fn a_sketch_hands_on_what_it_draws_and_builds_nothing_when_off() {
    let off = Sketch::default();
    off.draw(|| panic!("built while off"));
    off.draw_all(|| -> Vec<Item> { panic!("built while off") });

    let drawn = Arc::new(Mutex::new(Vec::new()));
    let sink = drawn.clone();
    let on = Sketch::new(move |item| sink.lock().unwrap().push(item));
    on.draw(|| Item::AskedTrain { trip: 1 });
    on.draw_all(|| [Item::AskedTrain { trip: 2 }, Item::AskedTrain { trip: 3 }]);
    let trips: Vec<Item> = drawn.lock().unwrap().clone();
    assert_eq!(trips, [1, 2, 3].map(|trip| Item::AskedTrain { trip }));
}

#[test]
fn ends_need_both_positions() {
    let stop = |name: &str, position| motis::Stop { id: name.to_owned(), name: name.to_owned(), position };
    let (a, b) = (stop("A", Some((49.479_354_9, 8.468_946))), stop("B", Some((-1.0, -2.000_004))));
    let ends = ends(&a, &b, at("08:00"), at("09:00"));
    let expected = Item::Ends {
        from: Place { name: "A".to_owned(), at: (49.47935, 8.46895) },
        to: Place { name: "B".to_owned(), at: (-1.0, -2.0) },
        depart_from: at("08:00").as_second(),
        depart_until: at("09:00").as_second(),
    };
    assert_eq!(ends, Some(expected));
    assert_eq!(super::ends(&a, &stop("C", None), at("08:00"), at("09:00")), None);
    assert_eq!(super::ends(&stop("C", None), &b, at("08:00"), at("09:00")), None);
}

#[test]
fn a_legs_train_has_its_stops_with_positions_live_times_first() {
    let from = motis_place("A", Some((50.123_456, 8.000_004)), ("", ""), ("08:00", "08:02"));
    let middle = motis_place("B", Some((50.5, 8.5)), ("08:30", ""), ("08:31", ""));
    let nowhere = motis_place("C", None, ("08:40", ""), ("08:41", ""));
    let to = motis_place("D", Some((51.0, 9.0)), ("09:00", "09:05"), ("", ""));
    let ride = leg(Some("trip-1"), from, vec![middle, nowhere], to);
    let expected = Item::Train {
        id: "trip-1".to_owned(),
        stops: vec![
            Stop(50.12346, 8.0, None, s("08:02")),
            Stop(50.5, 8.5, s("08:30"), s("08:31")),
            Stop(51.0, 9.0, s("09:05"), None),
        ],
    };
    assert_eq!(train(&ride), Some(expected.clone()));

    let walk = leg(
        None,
        motis_place("D", Some((51.0, 9.0)), ("", ""), ("", "")),
        vec![],
        motis_place("E", None, ("", ""), ("", "")),
    );
    let itinerary = |legs| Itinerary { legs };
    let other = leg(
        Some("trip-2"),
        motis_place("E", Some((1.0, 2.0)), ("", ""), ("", "")),
        vec![],
        motis_place("F", None, ("", ""), ("", "")),
    );
    let found = itineraries(&[itinerary(vec![ride, walk]), itinerary(vec![other])]);
    let trip_2 = Item::Train { id: "trip-2".to_owned(), stops: vec![Stop(1.0, 2.0, None, None)] };
    assert_eq!(found, [expected, trip_2], "walks have no train");
}

#[test]
fn onward_asks_at_the_center_else_where_the_first_train_boards() {
    let ride = |id: &str, name: &str, p: (f64, f64)| {
        leg(
            Some(id),
            motis_place(name, Some(p), ("", ""), ("08:00", "")),
            vec![],
            motis_place("Z", None, ("", ""), ("", "")),
        )
    };
    let legs = [ride("t1", "Alvesta", (56.9, 14.5)), ride("t2", "Other", (1.0, 1.0))];
    let train_of =
        |id: &str, p: (f64, f64)| Item::Train { id: id.to_owned(), stops: vec![Stop(p.0, p.1, None, s("08:00"))] };
    let asked = |name: &str, p: (f64, f64)| Item::Asked {
        place: Place { name: name.to_owned(), at: p },
        span: Some((at("08:00").as_second(), at("10:00").as_second())),
        seen: Vec::new(),
    };
    assert_eq!(
        onward(Some((56.899_999_9, 14.6)), at("08:00"), at("10:00"), &legs),
        [asked("Alvesta", (56.9, 14.6)), train_of("t1", (56.9, 14.5)), train_of("t2", (1.0, 1.0))]
    );
    assert_eq!(onward(None, at("08:00"), at("10:00"), &legs).first(), Some(&asked("Alvesta", (56.9, 14.5))));
    assert_eq!(onward(Some((2.0, 3.0)), at("08:00"), at("10:00"), &[]), [asked("", (2.0, 3.0))]);
    assert_eq!(onward(None, at("08:00"), at("10:00"), &[]), []);
}

fn station(name: &str, position: Option<(f64, f64)>) -> crate::timetable::Station {
    crate::timetable::Station { name: name.to_owned(), eva: None, tv: None, board_name: None, position, motis_id: None }
}

#[test]
fn a_db_board_hour_lists_its_trains_live_times_first() {
    let event = |planned: &str, changed: &str| db::Event {
        planned: (!planned.is_empty()).then(|| at(planned)),
        changed: (!changed.is_empty()).then(|| at(changed)),
        ..db::Event::default()
    };
    let visits = [
        Visit {
            trip: "1-2-3".to_owned(),
            arrival: Some(event("08:10", "08:12")),
            departure: Some(event("08:11", "")),
            ..Visit::default()
        },
        Visit { trip: "4-5-6".to_owned(), departure: Some(event("08:40", "08:41")), ..Visit::default() },
    ];
    let here = station("Frankfurt Süd", Some((50.099_224, 8.686_406)));
    let expected = Item::Asked {
        place: Place { name: "Frankfurt Süd".to_owned(), at: (50.09922, 8.68641) },
        span: Some((at("08:00").as_second(), at("09:00").as_second())),
        seen: vec![Seen("db:1-2-3".to_owned(), s("08:12"), s("08:11")), Seen("db:4-5-6".to_owned(), None, s("08:41"))],
    };
    assert_eq!(board_hour(&here, at("08:00"), &visits), Some(expected));
    assert_eq!(board_hour(&station("Nowhere", None), at("08:00"), &visits), None);
}

fn tv_station(signature: &str, position: Option<(f64, f64)>) -> trafikverket::Station {
    trafikverket::Station {
        signature: signature.to_owned(),
        name: format!("{signature} station"),
        country: "SE".to_owned(),
        position,
    }
}

fn run_key(ident: &str) -> trafikverket::RunKey {
    trafikverket::RunKey { ident: ident.to_owned(), date: "2026-10-24".to_owned() }
}

#[test]
fn a_trafikverket_board_lists_its_runs_at_their_planned_time() {
    let seen = [trafikverket::Seen {
        run: run_key("319"),
        planned: at("08:05"),
        class: crate::timetable::ProductClass::Regional,
    }];
    let expected = Item::Asked {
        place: Place { name: "G station".to_owned(), at: (57.7, 11.97) },
        span: Some((at("07:30").as_second(), at("10:30").as_second())),
        seen: vec![Seen("tv:319/2026-10-24".to_owned(), s("08:05"), s("08:05"))],
    };
    let span = (at("07:30"), at("10:30"));
    assert_eq!(tv_board(&tv_station("G", Some((57.7, 11.97))), span, &seen), Some(expected));
    assert_eq!(tv_board(&tv_station("G", None), span, &seen), None);
}

#[test]
fn a_trafikverket_run_has_its_stops_at_known_stations_live_times_first() {
    let moment = |planned: &str, live: &str| {
        Some(trafikverket::Moment {
            planned: at(planned),
            live: (!live.is_empty()).then(|| at(live)),
            cancelled: false,
        })
    };
    let stop = |signature: &str, arrival, departure| trafikverket::Stop {
        signature: signature.to_owned(),
        arrival,
        departure,
        track: None,
    };
    let run = trafikverket::Run {
        key: run_key("1036"),
        product: None,
        class: crate::timetable::ProductClass::Regional,
        stops: vec![
            stop("G", None, moment("08:00", "08:03")),
            stop("X", moment("08:20", ""), moment("08:21", "")),
            stop("Av", moment("09:00", ""), None),
        ],
    };
    let position = |sig: &str| match sig {
        "G" => Some((57.708_874_4, 11.973_454)),
        "Av" => Some((56.9, 14.55)),
        _ => None,
    };
    let expected = Item::Train {
        id: "tv:1036/2026-10-24".to_owned(),
        stops: vec![Stop(57.70887, 11.97345, None, s("08:03")), Stop(56.9, 14.55, s("09:00"), None)],
    };
    assert_eq!(tv_run(&run, position), expected);
}

#[test]
fn the_timetable_has_every_station_by_index_and_its_trains_live_times() {
    let mut b = Builder::new();
    b.trip("ICE 1", &[("A", "", "08:00+2"), ("B", "09:00+4", "09:02"), ("C", "10:00", "")]);
    b.trip("RE 2", &[("B", "", "09:10"), ("C", "09:50", "")]);
    let mut tt = b.build("A", "C");
    tt.stations[0].position = Some((50.000_004, 8.123_456));
    let item = timetable(&tt, &[Some(120), None]);
    let named = |name: &str, at, height| Station { name: name.to_owned(), at, height };
    let expected = Item::Timetable {
        stations: vec![named("A", Some((50.0, 8.12346)), Some(120)), named("B", None, None), named("C", None, None)],
        trains: vec![
            vec![Call(0, None, s("08:02")), Call(1, s("09:04"), s("09:02")), Call(2, s("10:00"), None)],
            vec![Call(1, None, s("09:10")), Call(2, s("09:50"), None)],
        ],
    };
    assert_eq!(item, expected, "heights past the given ones are unknown");
}
