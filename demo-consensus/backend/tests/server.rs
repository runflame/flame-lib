#![cfg(unix)]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use demo_consensus_backend::types::DemoSnapshot;
use serde_json::Value;

struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn get(address: SocketAddr, path: &str) -> Result<Value> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .context("incomplete HTTP response")?;
    ensure!(
        headers.starts_with("HTTP/1.1 200"),
        "unexpected response: {response}"
    );
    Ok(serde_json::from_str(body)?)
}

#[test]
fn executable_serves_http_and_stops_on_sigterm() -> Result<()> {
    let reservation = TcpListener::bind("127.0.0.1:0")?;
    let address = reservation.local_addr()?;
    drop(reservation);
    let mut server = ServerProcess(
        Command::new(env!("CARGO_BIN_EXE_demo-consensus-backend"))
            .env("DEMO_LISTEN", address.to_string())
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        ensure!(
            server.0.try_wait()?.is_none(),
            "server exited before becoming ready"
        );
        if let Ok(health) = get(address, "/api/health") {
            ensure!(
                health["status"] == "ready",
                "server started without consensus"
            );
            break;
        }
        ensure!(Instant::now() < deadline, "server did not become ready");
        std::thread::sleep(Duration::from_millis(50));
    }
    let snapshot: DemoSnapshot = serde_json::from_value(get(address, "/api/state")?)?;
    ensure!(
        snapshot.flame.blocks.len() == 1,
        "server omitted initial Flame block"
    );
    ensure!(
        snapshot.bitcoin.tip == snapshot.consensus.btc_cursor,
        "server snapshot is inconsistent"
    );
    let signal = Command::new("kill")
        .args(["-TERM", &server.0.id().to_string()])
        .status()?;
    ensure!(signal.success(), "could not signal demo process");
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(status) = server.0.try_wait()? {
            ensure!(status.success(), "demo shutdown failed: {status}");
            break;
        }
        ensure!(Instant::now() < deadline, "demo did not stop after SIGTERM");
        std::thread::sleep(Duration::from_millis(50));
    }
    ensure!(
        TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err(),
        "HTTP listener survived shutdown"
    );
    Ok(())
}
