// logtool-daemon — 系统日志分析守护进程
//
// 监听 Unix Socket，接收 CLI 发送的分析请求。
// 每个连接在独立线程中处理，避免慢请求阻塞其他客户端。
//
// 使用方式：
//   sudo logtool-daemon              # 前台运行（systemd 管理）
//   sudo logtool-daemon --foreground # 同上（显式前台）

use logtool::{
    Config, ErrorResponse, RunMode, SOCKET_PATH, analyze_journal, daemon_error_with_details,
    stream_journal_to_writer, validate_config, write_json_line,
};
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs, process};

const MAX_ACTIVE_CLIENTS: usize = 64;
const SOCKET_GROUP: &str = "logtool";
const REQUEST_LINE_MAX_BYTES: usize = 64 * 1024;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
const INCOMING_ERROR_BACKOFF: Duration = Duration::from_millis(100);

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    let foreground = args.iter().any(|a| a == "--foreground" || a == "-F");
    let show_help = args.iter().any(|a| a == "--help" || a == "-h");

    if show_help {
        println!("{}", daemon_help_text());
        return;
    }

    if !foreground {
        eprintln!("提示：守护进程以前台模式启动（使用 systemd 管理时无需 --foreground）");
    }

    if let Err(err) = run_daemon() {
        eprintln!("错误：{err}");
        process::exit(1);
    }
}

fn run_daemon() -> Result<(), String> {
    // 清理可能残留的 socket 文件
    let _ = fs::remove_file(SOCKET_PATH);

    let listener = UnixListener::bind(SOCKET_PATH).map_err(|err| {
        format!("无法绑定 Unix Socket {SOCKET_PATH}：{err}\n提示：可能需要 sudo 权限")
    })?;

    // 设置 socket 权限：仅 owner(root) 和同组用户可访问
    // 建议创建专用 logtool 组并将使用者加入该组
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o660);
        let _ = fs::set_permissions(SOCKET_PATH, perms);
    }

    if let Err(err) = try_set_socket_group(SOCKET_GROUP) {
        eprintln!("提示：{err}");
        eprintln!("   将回退为仅 root/当前组用户可访问 Socket。");
    }

    eprintln!("🚀 logtool 守护进程已启动，监听：{SOCKET_PATH}");
    eprintln!("   Socket 权限：0660（owner + group）");
    eprintln!("   Socket 组：{SOCKET_GROUP}（若存在）");
    eprintln!("   最大并发请求：{MAX_ACTIVE_CLIENTS}");
    warn_if_journal_not_persistent();

    let active_clients = Arc::new(AtomicUsize::new(0));

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let previous = active_clients.fetch_add(1, Ordering::AcqRel);
                if previous >= MAX_ACTIVE_CLIENTS {
                    active_clients.fetch_sub(1, Ordering::AcqRel);
                    let payload = daemon_busy_payload();
                    let _ = send_error_response(
                        &mut stream,
                        &payload.error,
                        payload.code.as_deref(),
                        payload.hint.as_deref(),
                    );
                    continue;
                }

                let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
                let active_clients = Arc::clone(&active_clients);
                // 每个连接在独立线程中处理，避免慢请求阻塞其他客户端
                thread::spawn(move || {
                    let _guard = ActiveClientGuard {
                        active_clients: Arc::clone(&active_clients),
                    };
                    let started = Instant::now();
                    let mut mode_for_log = None;
                    let result = handle_client(request_id, stream, &mut mode_for_log);
                    let duration_ms = started.elapsed().as_millis();
                    let mode = mode_for_log
                        .as_ref()
                        .map(run_mode_label)
                        .unwrap_or("unknown");

                    match result {
                        Ok(()) => {
                            eprintln!(
                                "request_id={request_id} mode={mode} duration_ms={duration_ms} result=ok"
                            );
                        }
                        Err(err) => {
                            eprintln!(
                                "request_id={request_id} mode={mode} duration_ms={duration_ms} result=error error={}",
                                sanitize_log_field(&err)
                            );
                        }
                    }
                });
            }
            Err(err) => {
                eprintln!("接受连接失败：{err}");
                thread::sleep(INCOMING_ERROR_BACKOFF);
            }
        }
    }

    Ok(())
}

fn handle_client(
    request_id: u64,
    stream: UnixStream,
    mode_for_log: &mut Option<RunMode>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(REQUEST_READ_TIMEOUT))
        .map_err(|e| format!("设置读取超时失败：{e}"))?;

    let read_stream = stream.try_clone().map_err(|e| e.to_string())?;
    let mut write_stream = stream;

    let mut buf_reader = BufReader::new(read_stream);

    // 读取一行 JSON 请求（带大小限制与超时保护）
    let request_line = match read_request_line(&mut buf_reader, REQUEST_LINE_MAX_BYTES) {
        Ok(None) => return Ok(()),
        Ok(Some(line)) => line,
        Err(read_error) => {
            let (message, code, hint) = match request_read_error_to_payload(&read_error) {
                Some(payload) => payload,
                None => {
                    let msg = format!("读取请求失败：{read_error:?}");
                    let _ = send_error_response(&mut write_stream, &msg, None, None);
                    return Err(msg);
                }
            };
            let _ = send_error_response(&mut write_stream, &message, Some(code), Some(hint));
            return Err(message);
        }
    };

    // 解析配置
    let config: Config = match serde_json::from_str(&request_line) {
        Ok(config) => config,
        Err(err) => {
            let msg = format!("解析请求 JSON 失败：{err}");
            let _ = send_error_response(
                &mut write_stream,
                &msg,
                Some("invalid_json"),
                Some("修复：请使用官方 CLI 发起请求，或运行：logtool --help"),
            );
            return Err(msg);
        }
    };
    *mode_for_log = Some(config.mode.clone());

    // 服务端参数校验，防止非法/恶意请求
    if let Err(err) = validate_config(&config) {
        let _ = send_error_response(
            &mut write_stream,
            &err,
            None,
            Some("修复：运行 logtool --help 查看支持参数组合"),
        );
        return Err(err);
    }

    eprintln!(
        "request_id={request_id} mode={} event=request_received since={:?} priority={} follow={}",
        run_mode_label(&config.mode),
        config.since,
        config.priority,
        config.follow
    );

    // 执行分析并返回结果
    let run_result = match config.mode {
        RunMode::Analyze => analyze_journal(&config)
            .and_then(|response| write_json_line(&mut write_stream, &response, "分析响应")),
        RunMode::Stream => {
            // 直接将 socket 作为 writer 传入，实现边读边发的真正流式输出
            stream_journal_to_writer(&config, &mut write_stream)
        }
    };

    if let Err(err) = run_result {
        let (code, hint) = runtime_error_metadata(&err);
        let _ = send_error_response(&mut write_stream, &err, code, hint.as_deref());
        return Err(err);
    }

    Ok(())
}

fn send_error_response(
    stream: &mut UnixStream,
    message: &str,
    code: Option<&str>,
    hint: Option<&str>,
) -> Result<(), String> {
    let payload = daemon_error_with_details(message.to_string(), code, hint.map(|v| v.to_string()));
    write_json_line(stream, &payload, "错误响应")
}

fn runtime_error_metadata(err: &str) -> (Option<&'static str>, Option<String>) {
    if err.contains("journalctl") {
        return (
            Some("journalctl_failed"),
            Some("修复：先运行 journalctl --version 检查可用性".to_string()),
        );
    }
    (None, None)
}

fn daemon_busy_payload() -> ErrorResponse {
    daemon_error_with_details(
        format!("守护进程繁忙：当前并发请求已达到上限 {MAX_ACTIVE_CLIENTS}"),
        Some("daemon_busy"),
        Some("修复：请稍后重试，或先运行 sudo systemctl status logtool --no-pager".to_string()),
    )
}

fn run_mode_label(mode: &RunMode) -> &'static str {
    match mode {
        RunMode::Analyze => "analyze",
        RunMode::Stream => "stream",
    }
}

fn sanitize_log_field(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

#[derive(Debug, PartialEq, Eq)]
enum RequestReadError {
    TooLarge,
    Timeout,
    InvalidUtf8,
    Io(String),
}

fn read_request_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Option<String>, RequestReadError> {
    let mut request_bytes = Vec::new();
    let mut limited_reader = reader.take((max_bytes + 1) as u64);
    let bytes_read = limited_reader
        .read_until(b'\n', &mut request_bytes)
        .map_err(classify_request_read_error)?;

    if bytes_read == 0 {
        return Ok(None);
    }

    if request_bytes.len() > max_bytes {
        return Err(RequestReadError::TooLarge);
    }

    if request_bytes.ends_with(b"\n") {
        request_bytes.pop();
        if request_bytes.ends_with(b"\r") {
            request_bytes.pop();
        }
    }

    let request = String::from_utf8(request_bytes).map_err(|_| RequestReadError::InvalidUtf8)?;
    let trimmed = request.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    Ok(Some(trimmed.to_string()))
}

fn classify_request_read_error(err: io::Error) -> RequestReadError {
    match err.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => RequestReadError::Timeout,
        _ => RequestReadError::Io(err.to_string()),
    }
}

fn request_read_error_to_payload(
    err: &RequestReadError,
) -> Option<(String, &'static str, &'static str)> {
    match err {
        RequestReadError::TooLarge => Some((
            format!("请求过大：单行请求不得超过 {REQUEST_LINE_MAX_BYTES} 字节"),
            "request_too_large",
            "修复：请缩短参数并重试，可先运行 logtool --help",
        )),
        RequestReadError::Timeout => Some((
            format!(
                "读取请求超时：{} 秒内未收到完整请求",
                REQUEST_READ_TIMEOUT.as_secs()
            ),
            "request_timeout",
            "修复：请重试；若频繁出现可运行 logtool doctor",
        )),
        RequestReadError::InvalidUtf8 => Some((
            "请求编码无效：仅支持 UTF-8 JSON".to_string(),
            "invalid_json",
            "修复：请使用官方 CLI 发起请求，或运行 logtool --help",
        )),
        RequestReadError::Io(_) => None,
    }
}

struct ActiveClientGuard {
    active_clients: Arc<AtomicUsize>,
}

impl Drop for ActiveClientGuard {
    fn drop(&mut self) {
        self.active_clients.fetch_sub(1, Ordering::AcqRel);
    }
}

fn try_set_socket_group(group: &str) -> Result<(), String> {
    let status = Command::new("chgrp")
        .arg(group)
        .arg(SOCKET_PATH)
        .status()
        .map_err(|e| format!("设置 Socket 组为 {group} 失败：{e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "设置 Socket 组为 {group} 失败，chgrp 退出码：{status}"
        ))
    }
}

fn daemon_help_text() -> &'static str {
    "logtool-daemon — 系统日志分析守护进程

用法：
  logtool-daemon [选项]

选项：
  -h, --help          显示此帮助信息
  -F, --foreground    前台运行（调试用，默认即前台）

说明：
  守护进程监听 Unix Socket（/run/logtool.sock），
  接收来自 logtool CLI 的分析请求并返回结果。
  每个连接在独立线程中处理，互不阻塞。

  Socket 权限为 0660（owner + group），需 root 或同组权限才能连接。
  启动时会尝试将 Socket 组设置为 logtool（如果该组存在）。

  建议通过 systemd 管理此服务：
    sudo systemctl start logtool
    sudo systemctl enable logtool
"
}

fn warn_if_journal_not_persistent() {
    if Path::new("/var/log/journal").is_dir() {
        return;
    }

    eprintln!("警告：未检测到 /var/log/journal，日志可能为 volatile（重启后丢失）");
    eprintln!("   建议启用持久化：");
    eprintln!("   1) sudo mkdir -p /var/log/journal");
    eprintln!("   2) 在 /etc/systemd/journald.conf 设置 Storage=persistent");
    eprintln!("   3) sudo systemctl restart systemd-journald");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_request_line_rejects_too_large_payload() {
        let payload = vec![b'a'; REQUEST_LINE_MAX_BYTES + 1];
        let mut reader = BufReader::new(Cursor::new(payload));
        let err = read_request_line(&mut reader, REQUEST_LINE_MAX_BYTES).expect_err("应失败");
        assert_eq!(err, RequestReadError::TooLarge);
    }

    #[test]
    fn read_request_line_returns_trimmed_json_line() {
        let payload = b"  {\"mode\":\"Analyze\"}\nrest".to_vec();
        let mut reader = BufReader::new(Cursor::new(payload));
        let line = read_request_line(&mut reader, REQUEST_LINE_MAX_BYTES)
            .expect("应成功")
            .expect("应有内容");
        assert_eq!(line, "{\"mode\":\"Analyze\"}");
    }

    #[test]
    fn classify_request_read_error_maps_timeout() {
        let timeout = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        assert_eq!(
            classify_request_read_error(timeout),
            RequestReadError::Timeout
        );
    }

    #[test]
    fn request_read_error_to_payload_maps_request_too_large() {
        let payload = request_read_error_to_payload(&RequestReadError::TooLarge).expect("应有映射");
        assert_eq!(payload.1, "request_too_large");
    }

    #[test]
    fn request_read_error_to_payload_maps_request_timeout() {
        let payload = request_read_error_to_payload(&RequestReadError::Timeout).expect("应有映射");
        assert_eq!(payload.1, "request_timeout");
    }

    #[test]
    fn daemon_busy_payload_contains_daemon_busy_code() {
        let payload = daemon_busy_payload();
        assert_eq!(payload.code.as_deref(), Some("daemon_busy"));
        assert!(payload.hint.is_some());
    }

    #[test]
    fn runtime_error_metadata_maps_journalctl_failure() {
        let (code, hint) = runtime_error_metadata("启动 journalctl 失败：missing");
        assert_eq!(code, Some("journalctl_failed"));
        assert!(hint.is_some());
    }
}
