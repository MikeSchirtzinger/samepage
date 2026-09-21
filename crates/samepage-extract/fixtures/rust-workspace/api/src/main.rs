use std::net::TcpListener;
use std::process::Command;

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("127.0.0.1:8080").expect("bind failed");

    tokio::spawn(background_ticker());

    let mut sidecar = Command::new("node").arg("sidecar.js").spawn().expect("failed to start sidecar");

    run_server(listener);
    let _ = sidecar.wait();
}

async fn background_ticker() {
    loop {
        tick().await;
    }
}

async fn tick() {}

fn run_server(_listener: TcpListener) {}
