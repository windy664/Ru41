use ru41::channels::qq::api::QqApiClient;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// 本地 mock：统计 POST /app/getAppAccessToken 次数并返回固定 token。
fn mock_token_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            assert!(String::from_utf8_lossy(&body).contains("clientSecret"));
            hits_clone.fetch_add(1, Ordering::SeqCst);
            let payload = r#"{"access_token":"mock-token","expires_in":"7200"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    (base, hits)
}

#[tokio::test]
async fn access_token_is_cached_and_refreshed_early() {
    let (base, hits) = mock_token_server();
    let client = QqApiClient::with_endpoints(
        "app-id",
        "secret",
        base.clone(),
        format!("{base}/app/getAppAccessToken"),
    );
    assert_eq!(client.access_token().await.unwrap(), "mock-token");
    assert_eq!(client.access_token().await.unwrap(), "mock-token");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn token_endpoint_error_is_reported() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let payload = r#"{"code":10001,"message":"bad secret"}"#;
            write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    let client = QqApiClient::with_endpoints(
        "app-id",
        "bad",
        base.clone(),
        format!("{base}/app/getAppAccessToken"),
    );
    let error = client.access_token().await.unwrap_err().to_string();
    assert!(error.contains("400"), "unexpected error: {error}");
}
