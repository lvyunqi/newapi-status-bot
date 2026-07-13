use std::ffi::c_void;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use abi_stable::std_types::RString;
use abi_stable_host_api::{
    HOST_API_V1_ABI_VERSION, HostApiV1, PluginInitConfig, PluginInitResult, ProactiveSendRequest,
    SendEnqueueStatus,
};
use libloading::Library;
use serde_json::json;

#[derive(Debug, PartialEq, Eq)]
struct Record {
    bot_id: String,
    target_kind: String,
    target_id: String,
    message: String,
}

unsafe extern "C" fn enqueue(context: *mut c_void, request: *const ProactiveSendRequest) -> i32 {
    if context.is_null() || request.is_null() {
        return SendEnqueueStatus::InvalidRequest.code();
    }
    // SAFETY: The harness keeps the record buffer alive until unbind returns.
    let records = unsafe { &*(context.cast::<Mutex<Vec<Record>>>()) };
    // SAFETY: The plugin keeps the request alive until this callback returns.
    let request = unsafe { &*request };
    records.lock().expect("record lock").push(Record {
        bot_id: request.bot_id.as_str().to_owned(),
        target_kind: request.target_kind.as_str().to_owned(),
        target_id: request.target_id.as_str().to_owned(),
        message: request.message.as_str().to_owned(),
    });
    SendEnqueueStatus::Accepted.code()
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!(
            "::error title=Linux FFI smoke panic::{}",
            escape_workflow_command(&info.to_string())
        );
    }));

    let library_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: newapi-status-ffi-smoke <dynamic-library>");
    let canonical_library = std::fs::canonicalize(&library_path).expect("canonical plugin path");
    let now = unix_now();
    let (base_url, server) = spawn_mock_newapi(now);
    let data_dir = std::env::temp_dir().join(format!(
        "newapi-status-ffi-smoke-{}-{}",
        std::process::id(),
        now
    ));
    std::fs::create_dir_all(&data_dir).expect("create smoke data directory");

    // SAFETY: This single-process smoke test owns the environment for its lifetime.
    unsafe { std::env::set_var("NEWAPI_STATUS_FFI_SMOKE_TOKEN", "test-token") };
    let config = smoke_config(&base_url);
    let records = Box::new(Mutex::new(Vec::<Record>::new()));
    let context = (&*records as *const Mutex<Vec<Record>>)
        .cast_mut()
        .cast::<c_void>();
    let host_api = HostApiV1 {
        abi_version: HOST_API_V1_ABI_VERSION,
        context,
        enqueue_send: Some(enqueue),
    };

    unsafe {
        let library = Library::new(&canonical_library).expect("load plugin");
        {
            let bind = library
                .get::<unsafe extern "C" fn(*const HostApiV1) -> i32>(
                    b"qimen_plugin_bind_host_api_v1",
                )
                .expect("bind symbol");
            let init = library
                .get::<unsafe extern "C" fn(PluginInitConfig) -> PluginInitResult>(
                    b"qimen_plugin_init",
                )
                .expect("init symbol");
            let shutdown = library
                .get::<unsafe extern "C" fn()>(b"qimen_plugin_shutdown")
                .expect("shutdown symbol");
            let unbind = library
                .get::<unsafe extern "C" fn() -> i32>(b"qimen_plugin_unbind_host_api_v1")
                .expect("unbind symbol");

            assert_eq!(
                SendEnqueueStatus::from_code(bind(&host_api)),
                SendEnqueueStatus::Accepted
            );
            let result = init(PluginInitConfig {
                plugin_id: RString::from("newapi-status-bot"),
                config_json: RString::from(config.as_str()),
                plugin_dir: RString::from(
                    canonical_library
                        .parent()
                        .and_then(Path::to_str)
                        .expect("plugin directory UTF-8"),
                ),
                data_dir: RString::from(data_dir.to_str().expect("data directory UTF-8")),
            });
            assert_eq!(
                result.code, 0,
                "plugin init failed: {}",
                result.error_message
            );

            let deadline = Instant::now() + Duration::from_secs(10);
            while records.lock().expect("record lock").is_empty() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            let captured = records.lock().expect("record lock");
            assert!(
                !captured.is_empty(),
                "collector did not proactively enqueue a report"
            );
            assert!(captured.iter().all(|record| {
                record.bot_id == "bot-a"
                    && record.target_kind == "group"
                    && record.target_id == "10001"
                    && record.message.contains("模型状态")
            }));
            let accepted_count = captured.len();
            drop(captured);

            shutdown();
            assert_eq!(
                SendEnqueueStatus::from_code(unbind()),
                SendEnqueueStatus::Accepted
            );
            thread::sleep(Duration::from_millis(100));
            assert_eq!(
                records.lock().expect("record lock").len(),
                accepted_count,
                "plugin enqueued after shutdown and Host API unbind"
            );
        }
        library.close().expect("close plugin library");
    }

    server.join().expect("mock New API server");
    report_mapping_state(&canonical_library);
    println!("Linux FFI load, proactive send, shutdown, unbind, and loader close verified");
}

fn smoke_config(base_url: &str) -> String {
    json!({
        "api": {
            "base_url": base_url,
            "admin_user_id": 3,
            "access_token_env": "NEWAPI_STATUS_FFI_SMOKE_TOKEN",
            "poll_interval_secs": 10,
            "initial_backfill_hours": 1,
            "overlap_secs": 0,
            "page_size": 100,
            "max_pages_per_model": 1,
            "request_timeout_secs": 5,
            "settlement_grace_secs": 1
        },
        "storage": {
            "database_file": "status.db",
            "retention_days": 1
        },
        "status": {
            "default_window": "1m",
            "minimum_samples": 1,
            "stale_after_secs": 180
        },
        "perf_metrics": {
            "enabled": false,
            "request_timeout_secs": 1
        },
        "bot": {
            "max_message_chars": 4000
        },
        "push": {
            "enabled": true,
            "mode": "periodic",
            "interval_secs": 30,
            "confirmations": 1,
            "cooldown_secs": 0,
            "targets": [{
                "bot_id": "bot-a",
                "kind": "group",
                "target_id": "10001"
            }]
        },
        "models": [{"name": "echo", "groups": ["default"]}]
    })
    .to_string()
}

fn spawn_mock_newapi(created_at: i64) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock New API");
    let address = listener.local_addr().expect("mock address");
    let handle = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept mock request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read timeout");
            let request = read_http_request(&mut stream).to_ascii_lowercase();
            assert!(request.contains("get /api/log/?"));
            assert!(request.contains("model_name=echo"));
            let body = json!({
                "success": true,
                "data": {
                    "total": 1,
                    "items": [{
                        "id": 1,
                        "created_at": created_at,
                        "type": 2,
                        "model_name": "echo",
                        "use_time": 1,
                        "channel": 1,
                        "group": "default",
                        "request_id": "ffi-smoke-request",
                        "other": {"frt": 100}
                    }]
                }
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write mock response");
        }
    });
    (format!("http://{address}"), handle)
}

fn read_http_request(stream: &mut impl Read) -> String {
    let mut request = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 1024];
    loop {
        let count = stream.read(&mut chunk).expect("read mock request");
        if count == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..count]);
        assert!(request.len() <= 64 * 1024, "mock request headers too large");
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).expect("mock request UTF-8")
}

fn report_mapping_state(library_path: &Path) {
    let maps = std::fs::read_to_string("/proc/self/maps").expect("read process maps");
    let path = library_path.to_str().expect("library path UTF-8");
    if maps.contains(path) {
        println!(
            "plugin mapping retained by the platform after successful loader close: {}",
            library_path.display()
        );
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn escape_workflow_command(message: &str) -> String {
    message
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}
