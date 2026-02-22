// logtool-daemon — 系统日志分析守护进程
//
// 监听 Unix Socket，接收 CLI 发送的分析请求。
// 每个连接在独立线程中处理，避免慢请求阻塞其他客户端。
//
// 使用方式：
//   sudo logtool-daemon              # 前台运行（systemd 管理）
//   sudo logtool-daemon --foreground # 同上（显式前台）

use logtool::{
    Config, RunMode, SOCKET_PATH, analyze_journal, daemon_error, stream_error_line,
    stream_journal_to_writer, validate_config, write_json_line,
};
use std::io::{BufRead, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::{env, fs, process};

const MAX_ACTIVE_CLIENTS: usize = 64;
const SOCKET_GROUP: &str = "logtool";

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
                    let busy = format!("守护进程繁忙：当前并发请求已达到上限 {MAX_ACTIVE_CLIENTS}");
                    let _ = send_error_response(&mut stream, None, &busy);
                    continue;
                }

                let active_clients = Arc::clone(&active_clients);
                // 每个连接在独立线程中处理，避免慢请求阻塞其他客户端
                thread::spawn(move || {
                    let _guard = ActiveClientGuard {
                        active_clients: Arc::clone(&active_clients),
                    };
                    if let Err(err) = handle_client(stream) {
                        eprintln!("处理客户端请求出错：{err}");
                    }
                });
            }
            Err(err) => {
                eprintln!("接受连接失败：{err}");
            }
        }
    }

    Ok(())
}

fn handle_client(stream: UnixStream) -> Result<(), String> {
    let read_stream = stream.try_clone().map_err(|e| e.to_string())?;
    let mut write_stream = stream;

    let mut buf_reader = BufReader::new(read_stream);

    // 读取一行 JSON 请求
    let mut request_line = String::new();
    buf_reader
        .read_line(&mut request_line)
        .map_err(|e| format!("读取请求失败：{e}"))?;

    let request_line = request_line.trim();
    if request_line.is_empty() {
        return Ok(());
    }

    // 解析配置
    let config: Config = match serde_json::from_str(request_line) {
        Ok(config) => config,
        Err(err) => {
            let msg = format!("解析请求 JSON 失败：{err}");
            let _ = send_error_response(&mut write_stream, None, &msg);
            return Err(msg);
        }
    };

    // 服务端参数校验，防止非法/恶意请求
    if let Err(err) = validate_config(&config) {
        let _ = send_error_response(&mut write_stream, Some(&config.mode), &err);
        return Err(err);
    }

    eprintln!(
        "收到请求：模式={:?}, since={:?}, priority={}, follow={}",
        config.mode, config.since, config.priority, config.follow
    );

    // 执行分析并返回结果
    let run_result = match config.mode {
        RunMode::Analyze => {
            let response = analyze_journal(&config)?;
            write_json_line(&mut write_stream, &response, "分析响应")
        }
        RunMode::Stream => {
            // 直接将 socket 作为 writer 传入，实现边读边发的真正流式输出
            stream_journal_to_writer(&config, &mut write_stream)
        }
    };

    if let Err(err) = run_result {
        let _ = send_error_response(&mut write_stream, Some(&config.mode), &err);
        return Err(err);
    }

    Ok(())
}

fn send_error_response(
    stream: &mut UnixStream,
    mode: Option<&RunMode>,
    message: &str,
) -> Result<(), String> {
    match mode {
        Some(RunMode::Stream) => {
            let line = stream_error_line(message.to_string());
            write_json_line(stream, &line, "流错误消息")
        }
        _ => {
            let payload = daemon_error(message.to_string());
            write_json_line(stream, &payload, "错误响应")
        }
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
