// Integration test for the `api` binary. Lives under `tests/`, so the
// scanner should skip this whole file rather than mistake its own
// throwaway listener for a real one.
use std::net::TcpListener;

#[test]
fn binds_a_listener_for_the_test_itself() {
    let _listener = TcpListener::bind("127.0.0.1:0").unwrap();
}
