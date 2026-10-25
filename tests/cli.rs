//! The binary as it's run: what `main` does with the command line.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;

/// A server answering every request with 500; returns its base URL.
#[expect(clippy::unwrap_used, reason = "test setup: failing is the point")]
fn failing_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut socket in listener.incoming().flatten() {
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
                line.clear();
            }
            socket
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .ok();
        }
    });
    base
}

#[test]
fn a_failing_search_exits_with_its_error() {
    let api = failing_server();
    let output = Command::new(env!("CARGO_BIN_EXE_umstieg"))
        .args(["Fulda", "Kassel", "--no-db", "--efa", "", "--api", &api, "--db-api", &api])
        .env_clear()
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("500"), "{stderr}");
    assert!(output.stdout.is_empty(), "no report");
}

#[test]
fn serve_listens_until_stopped() {
    let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let mut server = Command::new(env!("CARGO_BIN_EXE_umstieg"))
        .args(["serve", "--addr", &addr.to_string(), "--api", &failing_server()])
        .env_clear()
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut started = String::new();
    BufReader::new(server.stderr.take().unwrap()).read_line(&mut started).unwrap();
    assert_eq!(started, format!("umstieg: route planner on http://{addr}/\n"));

    let mut socket = std::net::TcpStream::connect(addr).unwrap();
    socket.write_all(b"GET / HTTP/1.1\r\nHost: umstieg\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    std::io::Read::read_to_string(&mut socket, &mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(server.try_wait().unwrap().is_none(), "still serving");
    server.kill().unwrap();
    server.wait().unwrap();
}
