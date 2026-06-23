use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_code_bridge_client::CodeBridgeClient;
use codex_code_bridge_client::ControlRequest;
use codex_code_bridge_client::ScreenshotRequest;
use codex_code_bridge_protocol::BridgeDescriptor;
use codex_code_bridge_protocol::BridgeEndpoint;
use codex_code_bridge_protocol::BridgeEvent;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientMetadata;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ConsoleEvent;
use codex_code_bridge_protocol::ControlCommand;
use codex_code_bridge_protocol::ControlResponseMessage;
use codex_code_bridge_protocol::EventKind;
use codex_code_bridge_protocol::EventPublishMessage;
use codex_code_bridge_protocol::PageviewEvent;
use codex_code_bridge_protocol::ScreenshotEvent;
use codex_code_bridge_protocol::ScreenshotMediaType;
use codex_code_bridge_protocol::ScreenshotPayload;
use codex_code_bridge_protocol::ScreenshotResponseMessage;
use codex_code_bridge_protocol::SourceKind;
use codex_code_bridge_protocol::SubscriptionFilter;
use image::ImageFormat;
use std::io::Read;
use std::io::Write;
use std::net::Ipv4Addr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const FIXTURE_HTML: &str = include_str!("fixtures/live_browser_witness.html");
const BROWSER_CLIENT_ID: &str = "live-browser-witness";

#[tokio::test]
#[ignore = "requires opening the printed URL in a real browser"]
async fn live_browser_witness_round_trips_events_screenshot_and_control() {
    let temp = TempDir::new().expect("temp home");
    let mut config = codex_code_bridge_service::BridgeServiceConfig::new(temp.path().to_path_buf());
    config.stale_client_timeout = Duration::from_secs(120);
    config.stale_client_sweep_interval = Duration::from_secs(120);
    let service = codex_code_bridge_service::start(config)
        .await
        .expect("start Code Bridge service");

    let descriptor = descriptor_from_path(service.descriptor_path());
    let fixture = LiveBrowserFixture::start(&descriptor);
    eprintln!(
        "Open this URL in a real browser to run the Code Bridge live browser witness:\n{}",
        fixture.url()
    );

    let client = CodeBridgeClient::from_descriptor(descriptor).expect("descriptor client");
    let subscriber = client
        .hello(
            "live-browser-witness-subscriber",
            ClientRole::Subscriber,
            CapabilitySet {
                subscribe_events: true,
                request_screenshot: true,
                request_control: true,
                ..CapabilitySet::default()
            },
            ClientMetadata {
                source_kind: SourceKind::TestFixture,
                label: Some("live browser witness subscriber".to_string()),
                provenance: None,
            },
        )
        .await
        .expect("subscriber hello");

    client
        .subscribe(
            &subscriber,
            SubscriptionFilter {
                levels: Vec::new(),
                event_kinds: vec![
                    EventKind::Console,
                    EventKind::Pageview,
                    EventKind::Screenshot,
                    EventKind::ControlResult,
                ],
                client_ids: vec![BROWSER_CLIENT_ID.to_string()],
            },
        )
        .await
        .expect("subscribe");
    let mut events = client.events(&subscriber, 0).await.expect("events");

    let pageview = next_message(&mut events, "pageview").await;
    assert!(matches!(
        pageview.envelope.payload,
        BridgePayload::Event(EventPublishMessage {
            event: BridgeEvent::Pageview(PageviewEvent { url, title }),
            ..
        }) if url.starts_with(&fixture.url())
            && title.as_deref() == Some("Code Bridge Live Browser Witness")
    ));

    let console = next_message(&mut events, "console").await;
    let event_sequence = console.sequence;
    assert!(matches!(
        console.envelope.payload,
        BridgePayload::Event(EventPublishMessage {
            event: BridgeEvent::Console(ConsoleEvent { text, .. }),
            ..
        }) if text == "live browser fixture ready"
    ));
    drop(events);

    client
        .request_screenshot(
            &subscriber,
            ScreenshotRequest {
                request_id: "live-browser-shot-1".to_string(),
                target_client_id: BROWSER_CLIENT_ID.to_string(),
                timeout_ms: 5_000,
            },
        )
        .await
        .expect("request screenshot");

    let mut events = client
        .events(&subscriber, event_sequence)
        .await
        .expect("events after screenshot request");
    let screenshot_response = next_message(&mut events, "screenshot response").await;
    let BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
        request_id,
        screenshot: Some(screenshot),
        ..
    }) = screenshot_response.envelope.payload
    else {
        panic!("expected screenshot response");
    };
    assert_eq!(request_id, "live-browser-shot-1");
    let screenshot_bytes = assert_nonblank_png(&screenshot);

    let screenshot_event = next_message(&mut events, "screenshot event").await;
    let control_cursor = screenshot_event.sequence;
    let BridgePayload::Event(EventPublishMessage {
        event:
            BridgeEvent::Screenshot(ScreenshotEvent {
                screenshot_id,
                media_type: ScreenshotMediaType::Png,
                bytes,
                ..
            }),
        ..
    }) = screenshot_event.envelope.payload
    else {
        panic!("expected screenshot event");
    };
    assert_eq!(screenshot_id, "live-browser-shot-1");
    assert_eq!(bytes, screenshot_bytes);
    drop(events);

    client
        .request_control(
            &subscriber,
            ControlRequest {
                request_id: "live-browser-js-1".to_string(),
                target_client_id: BROWSER_CLIENT_ID.to_string(),
                command: ControlCommand::ExecuteJavascript {
                    code: "window.location.href".to_string(),
                },
                timeout_ms: 5_000,
            },
        )
        .await
        .expect("request control");
    let mut events = client
        .events(&subscriber, control_cursor)
        .await
        .expect("events after control request");
    let control_response = next_message(&mut events, "control response").await;
    assert!(matches!(
        control_response.envelope.payload,
        BridgePayload::ControlResponse(ControlResponseMessage { request_id, summary, result, .. })
            if request_id == "live-browser-js-1"
                && summary.starts_with(&fixture.url())
                && result == Some(serde_json::json!(summary))
    ));

    service.shutdown().await;
}

async fn next_message(
    events: &mut codex_code_bridge_client::CodeBridgeEventStream,
    label: &str,
) -> codex_code_bridge_service::BridgeSseMessage {
    timeout(Duration::from_secs(30), events.next_message())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
        .unwrap_or_else(|error| panic!("failed waiting for {label}: {error}"))
}

fn assert_nonblank_png(screenshot: &ScreenshotPayload) -> usize {
    assert_eq!(screenshot.media_type, ScreenshotMediaType::Png);
    let bytes = BASE64_STANDARD
        .decode(screenshot.data_base64.as_bytes())
        .expect("decode screenshot");
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .expect("parse screenshot")
        .to_rgba8();
    assert_eq!(image.width(), screenshot.width);
    assert_eq!(image.height(), screenshot.height);
    let first = image
        .pixels()
        .next()
        .expect("screenshot has at least one pixel");
    assert!(image.pixels().any(|pixel| pixel != first));
    bytes.len()
}

fn descriptor_from_path(path: &std::path::Path) -> BridgeDescriptor {
    let raw = std::fs::read(path).expect("read descriptor");
    serde_json::from_slice(&raw).expect("parse descriptor")
}

struct LiveBrowserFixture {
    url: String,
    _thread: thread::JoinHandle<()>,
}

impl LiveBrowserFixture {
    fn start(descriptor: &BridgeDescriptor) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind fixture server");
        let url = format!(
            "http://127.0.0.1:{}/",
            listener.local_addr().expect("fixture addr").port()
        );
        let descriptor = Arc::new(descriptor.clone());
        let thread_descriptor = Arc::clone(&descriptor);
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(mut stream) => serve_fixture_request(&mut stream, &thread_descriptor),
                    Err(_) => break,
                }
            }
        });
        Self {
            url,
            _thread: thread,
        }
    }

    fn url(&self) -> String {
        self.url.clone()
    }
}

fn serve_fixture_request(stream: &mut TcpStream, descriptor: &BridgeDescriptor) {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    while buffer.len() < 8192 {
        let Ok(read) = stream.read(&mut chunk) else {
            return;
        };
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buffer);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if path == "/config.json" {
        let body = fixture_config_json(descriptor);
        write_response(stream, "application/json", &body);
    } else {
        write_response(stream, "text/html; charset=utf-8", FIXTURE_HTML);
    }
}

fn fixture_config_json(descriptor: &BridgeDescriptor) -> String {
    let endpoint_url = match &descriptor.endpoint {
        BridgeEndpoint::LoopbackHttp { url } => url,
        endpoint => panic!("expected loopback http endpoint, got {endpoint:?}"),
    };
    serde_json::json!({
        "endpointUrl": endpoint_url,
        "authSecret": descriptor.auth_secret,
    })
    .to_string()
}

fn write_response(stream: &mut TcpStream, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}
