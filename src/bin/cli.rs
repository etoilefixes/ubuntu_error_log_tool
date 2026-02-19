// logtool — 系统日志分析 CLI 客户端
//
// 通过 Unix Socket 连接 logtool-daemon 守护进程，
// 发送分析请求并展示中文结果。
//
// 使用方式：
//   logtool                                  # 默认分析最近 2 小时错误
//   logtool --since "30 min ago" --top 15    # 自定义时间范围
//   logtool --stream --follow --unit ssh     # 流模式查看

use logtool::{
    Action, AnalyzeResponse, Config, ErrorResponse, RunMode, SOCKET_PATH, StreamLine, help_text,
    parse_args, print_analysis_report,
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::{env, process};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let action = match parse_args(&args) {
        Ok(action) => action,
        Err(err) => {
            eprintln!("错误：{err}");
            process::exit(1);
        }
    };

    match action {
        Action::Help => {
            println!("{}", help_text());
        }
        Action::Run(config) => {
            if let Err(err) = send_request(&config) {
                eprintln!("错误：{err}");
                process::exit(1);
            }
        }
    }
}

fn send_request(config: &Config) -> Result<(), String> {
    // 连接守护进程
    let mut stream = UnixStream::connect(SOCKET_PATH).map_err(|err| {
        format!(
             "无法连接到 logtool 守护进程（{SOCKET_PATH}）：{err}\n\n\
             可能的原因：\n\
             1. 守护进程未启动 → 运行：sudo systemctl start logtool\n\
             2. 权限不足 → 运行：sudo logtool，或将用户加入专用 logtool 组\n\
             3. 首次使用 → 先安装服务：sudo cp logtool.service /etc/systemd/system/ && sudo systemctl start logtool"
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
