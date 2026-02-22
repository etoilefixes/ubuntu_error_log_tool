// logtool — 系统日志分析 CLI 客户端
//
// 通过 Unix Socket 连接 logtool-daemon 守护进程，
// 发送分析请求并展示中文结果。
//
// 使用方式：
//   logtool                                  # 进入交互模式
//   logtool> help                            # 查看帮助
//   logtool --since "30 min ago" --top 15    # 自定义时间范围
//   logtool --stream --follow --unit ssh     # 流模式查看
//   logtool doctor                            # 运行环境自检
//   logtool boots                             # 查看启动周期列表

use logtool::{
    Action, AnalyzeResponse, Config, ErrorResponse, RunMode, SOCKET_PATH, StreamLine, help_text,
    parse_args, print_analysis_report,
};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::{env, process};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = if args.is_empty() {
        run_interactive_shell()
    } else {
        run_single_command(args)
    };

    if let Err(err) = result {
        eprintln!("错误：{err}");
        process::exit(1);
    }
}

fn run_single_command(raw_args: Vec<String>) -> Result<(), String> {
    let args = normalize_command_aliases(raw_args);
    let action = parse_args(&args)?;
    execute_action(action)
}

fn execute_action(action: Action) -> Result<(), String> {
    match action {
        Action::Help => {
            println!("{}", help_text());
            Ok(())
        }
        Action::Version => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Action::Doctor => run_doctor(),
        Action::ListBoots => print_boot_list(),
        Action::Run(config) => send_request(&config),
    }
}

fn run_interactive_shell() -> Result<(), String> {
    println!("进入 logtool 交互模式。输入 help 查看命令，输入 exit 退出。");

    let stdin = io::stdin();
    let mut line = String::new();

    loop {
        print!("logtool> ");
        io::stdout()
            .flush()
            .map_err(|e| format!("写入提示符失败：{e}"))?;

        line.clear();
        let read = stdin
            .read_line(&mut line)
            .map_err(|e| format!("读取交互输入失败：{e}"))?;

        if read == 0 {
            println!();
            break;
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        if matches!(input, "exit" | "quit" | "q") {
            break;
        }

        let args = match split_interactive_line(input) {
            Ok(args) => args,
            Err(err) => {
                eprintln!("错误：{err}");
                continue;
            }
        };

        if args.is_empty() {
            continue;
        }

        if let Err(err) = run_single_command(args) {
            eprintln!("错误：{err}");
        }
    }

    Ok(())
}

fn normalize_command_aliases(raw_args: Vec<String>) -> Vec<String> {
    let mut iter = raw_args.into_iter();
    let Some(first) = iter.next() else {
        return Vec::new();
    };

    match first.as_str() {
        "analyze" => {
            let mut out = vec!["--analyze".to_string()];
            out.extend(iter);
            out
        }
        "stream" => {
            let mut out = vec!["--stream".to_string()];
            out.extend(iter);
            out
        }
        "run" => iter.collect(),
        _ => {
            let mut out = vec![first];
            out.extend(iter);
            out
        }
    }
}

fn split_interactive_line(line: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars();

    while let Some(ch) = chars.next() {
        match quote {
            Some(delimiter) => {
                if ch == delimiter {
                    quote = None;
                    continue;
                }

                if ch == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    } else {
                        current.push(ch);
                    }
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    } else {
                        current.push(ch);
                    }
                }
                c if c.is_whitespace() => {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if quote.is_some() {
        return Err("命令存在未闭合引号".to_string());
    }

    if !current.is_empty() {
        args.push(current);
    }

    Ok(args)
}

fn send_request(config: &Config) -> Result<(), String> {
    // 连接守护进程
    let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|err| {
        format!(
             "无法连接到 logtool 守护进程（{SOCKET_PATH}）：{err}\n\n\
             可能的原因：\n\
             1. 守护进程未启动 → 运行：sudo systemctl start logtool\n\
             2. 权限不足（未加入组）→ 运行：sudo usermod -aG logtool $USER\n\
             3. 权限不足（组已加入但当前会话未生效）→ 运行：newgrp logtool（或注销后重新登录）\n\
             4. 首次使用 → 先安装服务：sudo cp logtool.service /etc/systemd/system/ && sudo systemctl start logtool"
        )
    })?;

    // 发送 JSON 请求
    let request_json = serde_json::to_string(config).map_err(|e| format!("序列化请求失败：{e}"))?;

    stream
        .write_all(request_json.as_bytes())
        .map_err(|e| format!("发送请求失败：{e}"))?;
    stream
        .write_all(b"\n")
        .map_err(|e| format!("发送换行符失败：{e}"))?;
    stream.flush().map_err(|e| format!("刷新请求失败：{e}"))?;

    // 读取响应
    match config.mode {
        RunMode::Analyze => handle_analyze_response(&stream),
        RunMode::Stream => handle_stream_response(&stream),
    }
}

fn handle_analyze_response(stream: &UnixStream) -> Result<(), String> {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    let response_line = lines
        .next()
        .ok_or_else(|| "守护进程无响应".to_string())?
        .map_err(|e| format!("读取响应失败：{e}"))?;

    let response: AnalyzeResponse = match serde_json::from_str(&response_line) {
        Ok(response) => response,
        Err(_) => {
            if let Ok(error) = serde_json::from_str::<ErrorResponse>(&response_line) {
                return Err(format!("守护进程返回错误：{}", error.error));
            }
            return Err("解析响应 JSON 失败：响应格式不受支持".to_string());
        }
    };

    print_analysis_report(&response);
    Ok(())
}

fn handle_stream_response(stream: &UnixStream) -> Result<(), String> {
    let reader = BufReader::new(stream);

    for maybe_line in reader.lines() {
        let line = maybe_line.map_err(|e| format!("读取流响应失败：{e}"))?;

        let msg: StreamLine = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(_) => {
                if let Ok(error) = serde_json::from_str::<ErrorResponse>(&line) {
                    return Err(format!("守护进程返回错误：{}", error.error));
                }
                return Err("解析流消息失败：响应格式不受支持".to_string());
            }
        };

        if let Some(error) = msg.error {
            return Err(format!("流式请求失败：{error}"));
        }

        if msg.done {
            break;
        }

        println!("{}", msg.line);
    }

    Ok(())
}

fn print_boot_list() -> Result<(), String> {
    let output = Command::new("journalctl")
        .arg("--no-pager")
        .arg("--list-boots")
        .output()
        .map_err(|e| format!("执行 journalctl --list-boots 失败：{e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(format!(
                "journalctl --list-boots 执行失败，退出状态：{}",
                output.status
            ));
        }
        return Err(format!("journalctl --list-boots 执行失败：{stderr}"));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        println!("未找到可用启动周期记录。");
    } else {
        print!("{text}");
    }
    Ok(())
}

fn run_doctor() -> Result<(), String> {
    println!("logtool doctor");
    println!(
        "版本：{} {}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );
    println!();

    check_journalctl()?;
    check_journal_persistence();
    check_user_access();
    check_socket_status();
    check_daemon_connection();

    println!();
    println!("建议：若重启后查不到旧日志，请先开启 journald 持久化（Storage=persistent）。");
    Ok(())
}

fn check_journalctl() -> Result<(), String> {
    let output = Command::new("journalctl")
        .arg("--version")
        .output()
        .map_err(|e| format!("无法执行 journalctl：{e}"))?;

    if output.status.success() {
        println!("[OK] journalctl 可用");
        Ok(())
    } else {
        Err("journalctl 存在但不可用".to_string())
    }
}

fn check_journal_persistence() {
    if Path::new("/var/log/journal").is_dir() {
        println!("[OK] 检测到 /var/log/journal（日志可跨重启保留）");
    } else {
        println!("[WARN] 未检测到 /var/log/journal（重启后日志可能丢失）");
        println!("       启用方式：sudo mkdir -p /var/log/journal");
        println!(
            "               sudo sed -i 's/^#\\?Storage=.*/Storage=persistent/' /etc/systemd/journald.conf"
        );
        println!("               sudo systemctl restart systemd-journald");
    }
}

fn check_user_access() {
    let uid_output = Command::new("id").arg("-u").output();
    let uid = uid_output.ok().and_then(|out| {
        if out.status.success() {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        } else {
            None
        }
    });

    if uid == Some(0) {
        println!("[OK] 当前用户为 root");
        return;
    }

    let groups_output = Command::new("id").arg("-nG").output();
    match groups_output {
        Ok(out) if out.status.success() => {
            let groups_text = String::from_utf8_lossy(&out.stdout);
            let has_group = groups_text.split_whitespace().any(|g| g == "logtool");
            if has_group {
                println!("[OK] 当前用户在 logtool 组内");
            } else {
                println!(
                    "[WARN] 当前用户不在 logtool 组内，可能无法访问 {}",
                    SOCKET_PATH
                );
                println!("       运行：sudo usermod -aG logtool $USER && newgrp logtool");
            }
        }
        _ => {
            println!("[WARN] 无法检测当前用户组信息（命令 id -nG 失败）");
        }
    }
}

fn check_socket_status() {
    match fs::metadata(SOCKET_PATH) {
        Ok(meta) => {
            let mode = meta.permissions().mode() & 0o777;
            let uid = meta.uid();
            let gid = meta.gid();
            println!(
                "[OK] 检测到 Socket：{}（mode={:o}, uid={}, gid={}）",
                SOCKET_PATH, mode, uid, gid
            );
            if mode != 0o660 {
                println!("[WARN] Socket 权限建议为 660，当前为 {:o}", mode);
            }
        }
        Err(_) => {
            println!(
                "[WARN] 未检测到 Socket：{}（守护进程可能未启动）",
                SOCKET_PATH
            );
            println!("       运行：sudo systemctl start logtool");
        }
    }
}

fn check_daemon_connection() {
    match UnixStream::connect(SOCKET_PATH) {
        Ok(_) => println!("[OK] 可连接到守护进程 Socket"),
        Err(err) => {
            println!("[WARN] 无法连接守护进程 Socket：{err}");
            println!("       运行：sudo systemctl status logtool --no-pager");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_interactive_line_keeps_quoted_value() {
        let args = split_interactive_line(r#"analyze --since "2 hours ago" --priority 4"#)
            .expect("解析应成功");
        assert_eq!(
            args,
            vec![
                "analyze".to_string(),
                "--since".to_string(),
                "2 hours ago".to_string(),
                "--priority".to_string(),
                "4".to_string()
            ]
        );
    }

    #[test]
    fn split_interactive_line_supports_escape() {
        let args = split_interactive_line(r#"analyze --grep disk\ error"#).expect("解析应成功");
        assert_eq!(
            args,
            vec![
                "analyze".to_string(),
                "--grep".to_string(),
                "disk error".to_string()
            ]
        );
    }

    #[test]
    fn split_interactive_line_rejects_unclosed_quote() {
        let err = split_interactive_line(r#"analyze --since "2 hours ago"#).expect_err("应失败");
        assert!(err.contains("未闭合引号"));
    }

    #[test]
    fn normalize_aliases_analyze_to_flag() {
        let args = normalize_command_aliases(vec![
            "analyze".to_string(),
            "--priority".to_string(),
            "4".to_string(),
        ]);
        assert_eq!(
            args,
            vec![
                "--analyze".to_string(),
                "--priority".to_string(),
                "4".to_string()
            ]
        );
    }

    #[test]
    fn normalize_aliases_run_maps_to_default_args() {
        let args = normalize_command_aliases(vec!["run".to_string()]);
        assert!(args.is_empty());
    }
}
