use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use qrcode::{Color, QrCode};
use rand::RngCore;
use serde::Deserialize;
use std::{fmt, time::Duration};

const CREATE_BIND_TASK_URL: &str = "https://q.qq.com/lite/create_bind_task";
const POLL_BIND_RESULT_URL: &str = "https://q.qq.com/lite/poll_bind_result";
const QR_CONNECT_URL: &str = "https://q.qq.com/qqbot/openclaw/connect.html";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_MAX_REFRESHES: u32 = 3;
const MAX_TEMPORARY_POLL_ERRORS: u32 = 3;
const QR_QUIET_ZONE: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QqOnboardTask {
    pub task_id: String,
    pub qr_url: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct QqOnboardResult {
    pub app_id: String,
    pub client_secret: String,
    pub user_openid: String,
}

impl fmt::Debug for QqOnboardResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QqOnboardResult")
            .field("app_id", &self.app_id)
            .field("client_secret", &"<redacted>")
            .field("user_openid", &self.user_openid)
            .finish()
    }
}

#[doc(hidden)]
pub trait IntoQqOnboardCallbackResult {
    fn into_callback_result(self) -> Result<()>;
}

impl IntoQqOnboardCallbackResult for () {
    fn into_callback_result(self) -> Result<()> {
        Ok(())
    }
}

impl IntoQqOnboardCallbackResult for Result<()> {
    fn into_callback_result(self) -> Result<()> {
        self
    }
}

pub struct QqOnboardClient {
    http: reqwest::Client,
    create_url: String,
    poll_url: String,
    timeout: Duration,
    poll_interval: Duration,
    max_refreshes: u32,
}

impl QqOnboardClient {
    pub fn new() -> Self {
        Self::with_endpoints(
            CREATE_BIND_TASK_URL,
            POLL_BIND_RESULT_URL,
            DEFAULT_TIMEOUT,
            DEFAULT_POLL_INTERVAL,
            DEFAULT_MAX_REFRESHES,
        )
    }

    pub fn with_endpoints(
        create_url: impl Into<String>,
        poll_url: impl Into<String>,
        timeout: Duration,
        poll_interval: Duration,
        max_refreshes: u32,
    ) -> Self {
        Self {
            http: crate::util::http_client(),
            create_url: create_url.into(),
            poll_url: poll_url.into(),
            timeout,
            poll_interval,
            max_refreshes,
        }
    }

    pub async fn run<F, R>(&self, mut on_task: F) -> Result<QqOnboardResult>
    where
        F: FnMut(&QqOnboardTask) -> R,
        R: IntoQqOnboardCallbackResult,
    {
        for refresh in 0..=self.max_refreshes {
            let mut key = [0_u8; 32];
            rand::rng().fill_bytes(&mut key);
            let task_id = self.create_bind_task(&BASE64.encode(key)).await?;
            let mut qr_url =
                reqwest::Url::parse(QR_CONNECT_URL).context("invalid QQ onboard QR connect URL")?;
            qr_url.query_pairs_mut().append_pair("task_id", &task_id);
            let task = QqOnboardTask {
                task_id,
                qr_url: qr_url.to_string(),
            };
            on_task(&task)
                .into_callback_result()
                .context("cannot display QQ onboard task")?;

            let deadline = tokio::time::Instant::now() + self.timeout;
            let mut temporary_errors = 0_u32;
            loop {
                if tokio::time::Instant::now() >= deadline {
                    if refresh == self.max_refreshes {
                        bail!("QQ onboard timed out");
                    }
                    break;
                }

                let poll =
                    match tokio::time::timeout_at(deadline, self.poll_bind_result(&task.task_id))
                        .await
                    {
                        Ok(result) => result?,
                        Err(_) => {
                            if refresh == self.max_refreshes {
                                bail!("QQ onboard timed out");
                            }
                            break;
                        }
                    };
                match poll {
                    PollOutcome::Pending => {}
                    PollOutcome::TemporaryError => {
                        temporary_errors += 1;
                        if temporary_errors >= MAX_TEMPORARY_POLL_ERRORS {
                            bail!("QQ onboard poll returned repeated temporary errors");
                        }
                    }
                    PollOutcome::Expired => {
                        if refresh == self.max_refreshes {
                            bail!(
                                "QQ onboard QR code expired after {} refreshes",
                                self.max_refreshes
                            );
                        }
                        break;
                    }
                    PollOutcome::Completed(data) => {
                        if data.bot_appid.is_empty() {
                            bail!("QQ onboard completed without an app id");
                        }
                        let client_secret = decrypt_secret(&data.bot_encrypt_secret, &key)?;
                        if client_secret.is_empty() {
                            bail!("QQ onboard completed with an empty client secret");
                        }
                        return Ok(QqOnboardResult {
                            app_id: data.bot_appid,
                            client_secret,
                            user_openid: data.user_openid,
                        });
                    }
                }

                let next_poll = (tokio::time::Instant::now() + self.poll_interval).min(deadline);
                tokio::time::sleep_until(next_poll).await;
            }
        }
        bail!("QQ onboard did not complete")
    }

    async fn create_bind_task(&self, key: &str) -> Result<String> {
        let response: CreateResponse = self
            .post_json(
                &self.create_url,
                &serde_json::json!({ "key": key }),
                "create bind task",
            )
            .await?;
        if response.retcode.unwrap_or(0) != 0 {
            bail!(
                "QQ onboard create bind task failed: {}",
                response.msg.as_deref().unwrap_or("unknown server error")
            );
        }
        let task_id = response
            .data
            .map(|data| data.task_id)
            .filter(|task_id| !task_id.is_empty())
            .context("QQ onboard create response missing task_id")?;
        Ok(task_id)
    }

    async fn poll_bind_result(&self, task_id: &str) -> Result<PollOutcome> {
        let response: PollResponse = self
            .post_json(
                &self.poll_url,
                &serde_json::json!({ "task_id": task_id }),
                "poll bind result",
            )
            .await?;
        if response.retcode.unwrap_or(0) != 0 {
            return Ok(PollOutcome::TemporaryError);
        }
        let data = response
            .data
            .context("QQ onboard poll response missing data")?;
        match data.status {
            0 | 1 => Ok(PollOutcome::Pending),
            2 => Ok(PollOutcome::Completed(data)),
            3 => Ok(PollOutcome::Expired),
            status => bail!("QQ onboard poll returned unknown status {status}"),
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
        operation: &str,
    ) -> Result<T> {
        self.http
            .post(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("cannot {operation} for QQ onboard"))?
            .error_for_status()
            .with_context(|| format!("QQ onboard {operation} was rejected"))?
            .json()
            .await
            .with_context(|| format!("invalid QQ onboard {operation} response"))
    }
}

impl Default for QqOnboardClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct CreateResponse {
    retcode: Option<i64>,
    msg: Option<String>,
    data: Option<CreateData>,
}

#[derive(Deserialize)]
struct CreateData {
    task_id: String,
}

#[derive(Deserialize)]
struct PollResponse {
    retcode: Option<i64>,
    data: Option<PollData>,
}

#[derive(Deserialize)]
struct PollData {
    #[serde(default)]
    status: u8,
    #[serde(default)]
    bot_appid: String,
    #[serde(default)]
    bot_encrypt_secret: String,
    #[serde(default)]
    user_openid: String,
}

enum PollOutcome {
    Pending,
    TemporaryError,
    Expired,
    Completed(PollData),
}

fn decrypt_secret(encrypted: &str, key: &[u8; 32]) -> Result<String> {
    let raw = BASE64
        .decode(encrypted)
        .context("QQ onboard encrypted secret is not valid base64")?;
    if raw.len() < 28 {
        bail!("QQ onboard encrypted secret is too short");
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("invalid QQ onboard AES key"))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&raw[..12]), &raw[12..])
        .map_err(|_| anyhow::anyhow!("cannot decrypt QQ onboard client secret"))?;
    String::from_utf8(plaintext).context("QQ onboard client secret is not valid UTF-8")
}

pub fn render_qr(text: &str) -> Result<String> {
    let code = QrCode::new(text.as_bytes()).context("cannot generate QQ onboard QR code")?;
    let module_count = code.width() + QR_QUIET_ZONE * 2;
    let mut rendered = String::new();
    for y in (0..module_count).step_by(2) {
        for x in 0..module_count {
            let top = qr_module_is_dark(&code, x, y);
            let bottom = qr_module_is_dark(&code, x, y + 1);
            rendered.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        if y + 2 < module_count {
            rendered.push('\n');
        }
    }
    Ok(rendered)
}

fn qr_module_is_dark(code: &QrCode, x: usize, y: usize) -> bool {
    let Some(x) = x.checked_sub(QR_QUIET_ZONE) else {
        return false;
    };
    let Some(y) = y.checked_sub(QR_QUIET_ZONE) else {
        return false;
    };
    if x >= code.width() || y >= code.width() {
        return false;
    }
    code[(x, y)] == Color::Dark
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

    const APP_ID: &str = "test-app-id";
    const CLIENT_SECRET: &str = "test-client-secret-that-must-not-leak";
    const USER_OPENID: &str = "test-user-openid";

    #[derive(Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    }

    impl RecordedRequest {
        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    struct MockResponse {
        status: &'static str,
        body: String,
    }

    impl MockResponse {
        fn json(body: Value) -> Self {
            Self {
                status: "200 OK",
                body: body.to_string(),
            }
        }
    }

    struct MockServer {
        base: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
    }

    impl MockServer {
        fn new<F>(handler: F) -> Self
        where
            F: Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&requests);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else {
                        break;
                    };
                    let request = read_request(&stream);
                    let response = handler(&request);
                    recorded.lock().unwrap().push(request);
                    if write!(
                        stream,
                        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response.status,
                        response.body.len(),
                        response.body
                    )
                    .is_err()
                    {
                        break;
                    }
                }
            });
            Self { base, requests }
        }

        fn client(
            &self,
            timeout: Duration,
            poll_interval: Duration,
            max_refreshes: u32,
        ) -> QqOnboardClient {
            QqOnboardClient::with_endpoints(
                format!("{}/create", self.base),
                format!("{}/poll", self.base),
                timeout,
                poll_interval,
                max_refreshes,
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
        loop {
            let mut line = String::new();
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

    fn encrypt_secret(key_base64: &str, plaintext: &str) -> String {
        let key = BASE64.decode(key_base64).unwrap();
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let nonce = [0x42_u8; 12];
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .unwrap();
        let mut raw = nonce.to_vec();
        raw.extend(ciphertext);
        BASE64.encode(raw)
    }

    #[tokio::test]
    async fn pending_then_completed_validates_requests_and_decrypts() {
        const TASK_ID: &str = "task /+?=雪";
        let key = Arc::new(Mutex::new(None::<String>));
        let polls = Arc::new(AtomicUsize::new(0));
        let server = MockServer::new({
            let key = Arc::clone(&key);
            let polls = Arc::clone(&polls);
            move |request| match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/create") => {
                    let request_key = request.json()["key"].as_str().unwrap().to_owned();
                    *key.lock().unwrap() = Some(request_key);
                    MockResponse::json(json!({
                        "retcode": 0,
                        "data": { "task_id": TASK_ID }
                    }))
                }
                ("POST", "/poll") => {
                    if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                        MockResponse::json(json!({
                            "retcode": 0,
                            "data": { "status": 1 }
                        }))
                    } else {
                        let encrypted =
                            encrypt_secret(key.lock().unwrap().as_deref().unwrap(), CLIENT_SECRET);
                        MockResponse::json(json!({
                            "retcode": 0,
                            "data": {
                                "status": 2,
                                "bot_appid": APP_ID,
                                "bot_encrypt_secret": encrypted,
                                "user_openid": USER_OPENID
                            }
                        }))
                    }
                }
                _ => MockResponse {
                    status: "404 Not Found",
                    body: "{}".into(),
                },
            }
        });
        let client = server.client(Duration::from_secs(1), Duration::ZERO, 0);
        let mut shown = Vec::new();
        let result = client
            .run(|task| {
                shown.push(task.clone());
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(result.app_id, APP_ID);
        assert_eq!(result.client_secret, CLIENT_SECRET);
        assert_eq!(result.user_openid, USER_OPENID);
        assert!(!format!("{result:?}").contains(CLIENT_SECRET));
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].task_id, TASK_ID);
        let qr_url = reqwest::Url::parse(&shown[0].qr_url).unwrap();
        assert_eq!(qr_url.as_str().split('?').next().unwrap(), QR_CONNECT_URL);
        assert_eq!(
            qr_url
                .query_pairs()
                .find(|(name, _)| name == "task_id")
                .unwrap()
                .1,
            TASK_ID
        );

        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/create");
        assert_eq!(requests[0].headers["accept"], "application/json");
        assert_eq!(requests[0].headers["content-type"], "application/json");
        let create_body = requests[0].json();
        let encoded_key = create_body["key"].as_str().unwrap();
        assert_eq!(BASE64.decode(encoded_key).unwrap().len(), 32);
        assert_eq!(create_body.as_object().unwrap().len(), 1);
        for request in &requests[1..] {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/poll");
            assert_eq!(request.json(), json!({ "task_id": TASK_ID }));
        }
    }

    #[tokio::test]
    async fn expired_task_is_refreshed_once_per_new_task() {
        let creates = Arc::new(AtomicUsize::new(0));
        let latest_key = Arc::new(Mutex::new(None::<String>));
        let server = MockServer::new({
            let creates = Arc::clone(&creates);
            let latest_key = Arc::clone(&latest_key);
            move |request| match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/create") => {
                    *latest_key.lock().unwrap() =
                        Some(request.json()["key"].as_str().unwrap().to_owned());
                    let number = creates.fetch_add(1, Ordering::SeqCst) + 1;
                    MockResponse::json(json!({
                        "data": { "task_id": format!("task-{number}") }
                    }))
                }
                ("POST", "/poll") => {
                    let task_id = request.json()["task_id"].as_str().unwrap().to_owned();
                    if task_id == "task-1" {
                        MockResponse::json(json!({ "data": { "status": 3 } }))
                    } else {
                        let encrypted = encrypt_secret(
                            latest_key.lock().unwrap().as_deref().unwrap(),
                            CLIENT_SECRET,
                        );
                        MockResponse::json(json!({
                            "data": {
                                "status": 2,
                                "bot_appid": APP_ID,
                                "bot_encrypt_secret": encrypted
                            }
                        }))
                    }
                }
                _ => MockResponse {
                    status: "404 Not Found",
                    body: "{}".into(),
                },
            }
        });
        let client = server.client(Duration::from_secs(1), Duration::ZERO, 1);
        let mut shown = Vec::new();
        let result = client
            .run(|task| {
                shown.push(task.clone());
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(result.client_secret, CLIENT_SECRET);
        assert!(result.user_openid.is_empty());
        assert_eq!(
            shown
                .iter()
                .map(|task| task.task_id.as_str())
                .collect::<Vec<_>>(),
            ["task-1", "task-2"]
        );
        assert_ne!(shown[0].qr_url, shown[1].qr_url);
        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].path, "/create");
        assert_eq!(requests[1].json(), json!({ "task_id": "task-1" }));
        assert_eq!(requests[2].path, "/create");
        assert_eq!(requests[3].json(), json!({ "task_id": "task-2" }));
    }

    #[tokio::test]
    async fn create_failure_does_not_show_or_poll_a_task() {
        let server = MockServer::new(|_| {
            MockResponse::json(json!({ "retcode": 10001, "msg": "create rejected" }))
        });
        let client = server.client(Duration::from_secs(1), Duration::ZERO, 3);
        let mut callbacks = 0;
        let error = client
            .run(|_| {
                callbacks += 1;
                Ok(())
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("create rejected"));
        assert_eq!(callbacks, 0);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn pending_task_times_out_without_unbounded_polling() {
        let server = MockServer::new(|request| {
            if request.path == "/create" {
                MockResponse::json(json!({ "data": { "task_id": "pending-task" } }))
            } else {
                MockResponse::json(json!({ "data": { "status": 0 } }))
            }
        });
        let client = server.client(Duration::from_millis(50), Duration::from_millis(1), 0);
        let error = client.run(|_| Ok(())).await.unwrap_err();

        assert!(error.to_string().contains("timed out"));
        let requests = server.requests();
        assert_eq!(requests[0].path, "/create");
        assert!(requests.len() > 1);
        assert!(requests.len() < 100);
    }

    #[tokio::test]
    async fn invalid_ciphertext_returns_no_partial_credentials_or_secret() {
        let server = MockServer::new(|request| {
            if request.path == "/create" {
                MockResponse::json(json!({ "data": { "task_id": "bad-ciphertext" } }))
            } else {
                MockResponse::json(json!({
                    "data": {
                        "status": 2,
                        "bot_appid": APP_ID,
                        "bot_encrypt_secret": BASE64.encode([0_u8; 28]),
                        "user_openid": USER_OPENID
                    }
                }))
            }
        });
        let client = server.client(Duration::from_secs(1), Duration::ZERO, 0);
        let error = client.run(|_| Ok(())).await.unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("cannot decrypt QQ onboard client secret"));
        assert!(!message.contains(CLIENT_SECRET));
        assert!(!message.contains(APP_ID));
        assert!(!message.contains(USER_OPENID));
    }

    #[tokio::test]
    async fn temporary_poll_retcode_has_a_retry_limit() {
        let server = MockServer::new(|request| {
            if request.path == "/create" {
                MockResponse::json(json!({ "data": { "task_id": "temporary-error" } }))
            } else {
                MockResponse::json(json!({ "retcode": 500, "msg": "try later" }))
            }
        });
        let client = server.client(Duration::from_secs(1), Duration::ZERO, 0);
        let error = client.run(|_| Ok(())).await.unwrap_err();

        assert!(error.to_string().contains("temporary errors"));
        assert_eq!(
            server.requests().len(),
            1 + MAX_TEMPORARY_POLL_ERRORS as usize
        );
    }

    #[test]
    fn qr_text_only_contains_two_row_terminal_art_and_preserves_modules() {
        let text = "https://q.qq.com/qqbot/openclaw/connect.html?task_id=qr-test";
        let rendered = render_qr(text).unwrap();
        let code = QrCode::new(text.as_bytes()).unwrap();
        let module_count = code.width() + QR_QUIET_ZONE * 2;
        let lines = rendered.split('\n').collect::<Vec<_>>();
        assert_eq!(lines.len(), module_count.div_ceil(2));
        assert!(!rendered.contains(text));

        for (line_index, line) in lines.iter().enumerate() {
            assert_eq!(line.chars().count(), module_count);
            for (x, cell) in line.chars().enumerate() {
                let (top, bottom) = match cell {
                    '█' => (true, true),
                    '▀' => (true, false),
                    '▄' => (false, true),
                    ' ' => (false, false),
                    other => panic!("unexpected QR cell {other:?}"),
                };
                for (row, actual) in [(line_index * 2, top), (line_index * 2 + 1, bottom)] {
                    let expected = if x >= QR_QUIET_ZONE
                        && x < QR_QUIET_ZONE + code.width()
                        && row >= QR_QUIET_ZONE
                        && row < QR_QUIET_ZONE + code.width()
                    {
                        code[(x - QR_QUIET_ZONE, row - QR_QUIET_ZONE)] == Color::Dark
                    } else {
                        false
                    };
                    assert_eq!(actual, expected, "QR module at ({x}, {row})");
                }
            }
        }
    }
}
