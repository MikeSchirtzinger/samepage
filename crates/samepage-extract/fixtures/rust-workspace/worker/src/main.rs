fn main() {
    println!("worker started");
}

// An old approach, kept only as a note: TcpListener::bind("127.0.0.1:9999")
// is not a real lane here, it's a comment.

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    #[test]
    fn a_bind_in_a_test_module_is_not_a_lane() {
        let _listener = TcpListener::bind("127.0.0.1:0").unwrap();
    }
}
