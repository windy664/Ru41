use super::{OpsOutcome, check};
use crate::{
    bootstrap::{assembly::qq_api, config::Config},
    channels::qq::api::QqApiClient,
    ingress::cli::{Cli, QqCredentials},
};

pub(crate) async fn run(cli: &Cli, credentials: &QqCredentials) -> OpsOutcome {
    let (config, _) = match Config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(error) => return OpsOutcome::failure(error.to_string()),
    };
    let app_id = credentials
        .app_id
        .clone()
        .or_else(|| config.channel.qq.app_id.clone())
        .unwrap_or_default();
    let app_secret = credentials
        .app_secret
        .clone()
        .or_else(|| config.channel.qq.app_secret.clone())
        .unwrap_or_default();
    diagnose(app_id, app_secret, || qq_api(credentials, &config)).await
}

async fn diagnose(
    app_id: String,
    app_secret: String,
    make_api: impl FnOnce() -> anyhow::Result<QqApiClient>,
) -> OpsOutcome {
    let mut checks = Vec::new();
    if app_id.trim().is_empty() || app_secret.trim().is_empty() {
        checks.push(check(
            "credentials",
            "error",
            "QQ app id and client secret are required",
        ));
        return OpsOutcome::report(checks)
            .with_field("websocket_connected", serde_json::json!(false))
            .with_field("message_sent", serde_json::json!(false));
    }
    checks.push(check("credentials", "ok", "credentials are configured"));
    let api = match make_api() {
        Ok(api) => api,
        Err(error) => {
            checks.push(check(
                "client",
                "error",
                sanitize_error(&error.to_string(), &[&app_id, &app_secret]),
            ));
            return OpsOutcome::report(checks);
        }
    };
    let token = match api.access_token().await {
        Ok(token) => {
            checks.push(check("access_token", "ok", "access token acquired"));
            token
        }
        Err(error) => {
            checks.push(check(
                "access_token",
                "error",
                sanitize_error(&error.to_string(), &[&app_id, &app_secret]),
            ));
            checks.push(check("gateway", "warning", "gateway check skipped"));
            return OpsOutcome::report(checks)
                .with_field("websocket_connected", serde_json::json!(false))
                .with_field("message_sent", serde_json::json!(false));
        }
    };
    match api.gateway_url().await {
        Ok(_) => checks.push(check("gateway", "ok", "gateway URL acquired")),
        Err(error) => checks.push(check(
            "gateway",
            "error",
            sanitize_error(&error.to_string(), &[&app_id, &app_secret, &token]),
        )),
    }
    OpsOutcome::report(checks)
        .with_field("websocket_connected", serde_json::json!(false))
        .with_field("message_sent", serde_json::json!(false))
}

fn sanitize_error(error: &str, secrets: &[&str]) -> String {
    let mut sanitized = error.to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            sanitized = sanitized.replace(secret, "<redacted>");
        }
    }
    sanitized.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{
        collections::HashMap,
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    const APP_ID: &str = "sensitive-app-id";
    const CLIENT_SECRET: &str = "sensitive-client-secret";
    const ACCESS_TOKEN: &str = "sensitive-access-token";

    #[derive(Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    struct MockQqServer {
        base: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
    }

    impl MockQqServer {
        fn new(token_succeeds: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let base = format!("http://{address}");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&requests);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    let request = read_request(&stream);
                    let (status, payload) = match (request.method.as_str(), request.path.as_str()) {
                        ("POST", "/app/getAppAccessToken") if token_succeeds => (
                            "200 OK",
                            format!(r#"{{"access_token":"{ACCESS_TOKEN}","expires_in":"7200"}}"#),
                        ),
                        ("POST", "/app/getAppAccessToken") => (
                            "400 Bad Request",
                            format!(
                                r#"{{"code":10001,"message":"invalid {APP_ID} {CLIENT_SECRET}"}}"#
                            ),
                        ),
                        ("GET", "/gateway") => {
                            ("200 OK", format!(r#"{{"url":"ws://{address}/websocket"}}"#))
                        }
                        _ => (
                            "404 Not Found",
                            r#"{"message":"unexpected request"}"#.into(),
                        ),
                    };
                    recorded.lock().unwrap().push(request);
                    write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    )
                    .unwrap();
                }
            });
            Self { base, requests }
        }

        fn client(&self) -> QqApiClient {
            QqApiClient::with_endpoints(
                APP_ID,
                CLIENT_SECRET,
                self.base.clone(),
                format!("{}/app/getAppAccessToken", self.base),
            )
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn read_request(stream: &std::net::TcpStream) -> RecordedRequest {
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap().to_owned();
        let path = parts.next().unwrap().to_owned();
        let mut headers = HashMap::new();
        let mut content_length = 0;
        let mut line = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim().to_owned();
            if name == "content-length" {
                content_length = value.parse().unwrap();
            }
            headers.insert(name, value);
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        RecordedRequest {
            method,
            path,
            headers,
            body,
        }
    }

    async fn diagnose_with(server: &MockQqServer) -> OpsOutcome {
        let client = server.client();
        diagnose(APP_ID.into(), CLIENT_SECRET.into(), || Ok(client)).await
    }

    fn assert_side_effects_disabled(outcome: &OpsOutcome) {
        assert_eq!(outcome.value["websocket_connected"], json!(false));
        assert_eq!(outcome.value["message_sent"], json!(false));
    }

    fn assert_secrets_absent(outcome: &OpsOutcome) {
        let output = serde_json::to_string(&outcome.value).unwrap();
        for secret in [APP_ID, CLIENT_SECRET, ACCESS_TOKEN] {
            assert!(!output.contains(secret), "report leaked {secret}: {output}");
        }
    }

    #[tokio::test]
    async fn diagnose_checks_token_and_gateway_without_side_effects() {
        let server = MockQqServer::new(true);

        let outcome = diagnose_with(&server).await;

        assert!(outcome.success);
        assert_eq!(outcome.value["ok"], json!(true));
        assert_side_effects_disabled(&outcome);
        assert_secrets_absent(&outcome);
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/app/getAppAccessToken");
        let token_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            token_body,
            json!({"appId": APP_ID, "clientSecret": CLIENT_SECRET})
        );
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].path, "/gateway");
        let authorization = format!("QQBot {ACCESS_TOKEN}");
        assert_eq!(
            requests[1].headers.get("authorization"),
            Some(&authorization)
        );
    }

    #[tokio::test]
    async fn token_failure_skips_gateway_and_redacts_credentials() {
        let server = MockQqServer::new(false);

        let outcome = diagnose_with(&server).await;

        assert!(!outcome.success);
        assert_eq!(outcome.value["ok"], json!(false));
        assert_eq!(outcome.value["checks"][1]["name"], json!("access_token"));
        assert_eq!(outcome.value["checks"][1]["status"], json!("error"));
        assert_eq!(outcome.value["checks"][2]["name"], json!("gateway"));
        assert_eq!(outcome.value["checks"][2]["status"], json!("warning"));
        assert_side_effects_disabled(&outcome);
        assert_secrets_absent(&outcome);
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/app/getAppAccessToken");
    }

    #[tokio::test]
    async fn empty_credentials_skip_client_and_network() {
        let server = MockQqServer::new(true);
        let client = server.client();
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::clone(&factory_calls);

        let outcome = diagnose(" ".into(), String::new(), move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(client)
        })
        .await;

        assert!(!outcome.success);
        assert_eq!(outcome.value["ok"], json!(false));
        assert_eq!(outcome.value["checks"][0]["name"], json!("credentials"));
        assert_eq!(outcome.value["checks"][0]["status"], json!("error"));
        assert_side_effects_disabled(&outcome);
        assert_eq!(factory_calls.load(Ordering::SeqCst), 0);
        assert!(server.requests().is_empty());
    }
}
