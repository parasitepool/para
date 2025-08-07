use super::*;

#[test]
fn run() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut child = CommandBuilder::new(format!("server --address 127.0.0.1 --port {port}"))
        .integration_test(true)
        .spawn();

    for attempt in 0.. {
        if let Ok(response) = reqwest::blocking::get(format!("http://127.0.0.1:{port}")) {
            if response.status() == 200 {
                break;
            }
        }

        if attempt == 100 {
            panic!("Server did not respond to status check",);
        }

        thread::sleep(Duration::from_millis(50));
    }

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn serve_stats() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut child = CommandBuilder::new(format!("server --address 127.0.0.1 --port {port}"))
        .integration_test(true)
        .spawn();
}

#[test]
fn stats_aggregator() {}
