// logtool 核心库 — 日志分析引擎
//
// 提供 journalctl 日志的解析、归因分析、包反查等功能。
// 被 daemon 和 CLI 共用。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};

pub const DEFAULT_SINCE: &str = "2 hours ago";
pub const DEFAULT_PRIORITY: &str = "3";
pub const DEFAULT_TOP: usize = 10;
pub const SOCKET_PATH: &str = "/run/logtool.sock";

// ── 配置与枚举 ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub mode: RunMode,
    pub since: Option<String>,
    pub until: Option<String>,
    pub units: Vec<String>,
    pub grep_terms: Vec<String>,
    pub boot: BootFilter,
    pub follow: bool,
    pub kernel_only: bool,
    pub output_json: bool,
    pub max_lines: Option<usize>,
    pub priority: String,
    pub show_command: bool,
    pub top: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunMode {
    Analyze,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootFilter {
    Disabled,
    Current,
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Run(Config),
    Help,
    Version,
    Doctor,
    ListBoots,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourceKind {
    Unit,
    Executable,
    Identifier,
    Comm,
    Kernel,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    pub message: String,
    pub priority: Option<u8>,
    pub unit: Option<String>,
    pub exe: Option<String>,
    pub comm: Option<String>,
    pub identifier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStats {
    pub kind: SourceKind,
    pub source: String,
    pub count: u64,
    pub worst_priority: u8,
    pub sample_message: String,
    pub sample_unit: Option<String>,
    pub sample_exe: Option<String>,
    pub package: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalyzeMetrics {
    pub lines_read: usize,
    pub parsed_ok: usize,
    pub matched: usize,
    pub parse_errors: usize,
}

/// daemon → CLI 的响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeResponse {
    pub metrics: AnalyzeMetrics,
    pub suspects: Vec<SourceStats>,
    pub top: usize,
}

/// stream 模式下 daemon → CLI 的逐行消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamLine {
    pub line: String,
    pub done: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// daemon → CLI 的统一错误响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: RunMode::Analyze,
            since: Some(DEFAULT_SINCE.to_string()),
            until: None,
            units: Vec::new(),
            grep_terms: Vec::new(),
            // 默认跨启动周期查询，避免“异常后重启就看不到”的常见排障盲区。
            boot: BootFilter::Disabled,
            follow: false,
            kernel_only: false,
            output_json: false,
            max_lines: Some(1500),
            priority: DEFAULT_PRIORITY.to_string(),
            show_command: false,
            top: DEFAULT_TOP,
        }
    }
}

// ── 参数解析 ─────────────────────────────────────────────

pub fn parse_args(args: &[String]) -> Result<Action, String> {
    let mut config = Config::default();
    let mut i = 0usize;
    let mut max_lines_explicit = false;

    while i < args.len() {
        let arg = &args[i];

        match arg.as_str() {
            "--help" | "-h" | "help" => return Ok(Action::Help),
            "--version" | "-V" | "-v" | "version" => {
                return standalone_action(args, arg, Action::Version);
            }
            "--doctor" | "doctor" => return standalone_action(args, arg, Action::Doctor),
            "--list-boots" | "boots" => {
                return standalone_action(args, arg, Action::ListBoots);
            }
            "--analyze" => config.mode = RunMode::Analyze,
            "--stream" => config.mode = RunMode::Stream,
            "--all-boots" => config.boot = BootFilter::Disabled,
            "--follow" | "-f" => config.follow = true,
            "--kernel" | "-k" => config.kernel_only = true,
            "--json" => config.output_json = true,
            "--show-command" => config.show_command = true,
            "--no-default-since" => config.since = None,
            "--since" => {
                let value = get_next_value(args, &mut i, "--since")?;
                config.since = Some(value);
            }
            "--until" => {
                let value = get_next_value(args, &mut i, "--until")?;
                config.until = Some(value);
            }
            "--unit" | "-u" => {
                let value = get_next_value(args, &mut i, "--unit")?;
                config.units.push(value);
            }
            "--grep" | "-g" => {
                let value = get_next_value(args, &mut i, "--grep")?;
                if !value.is_empty() {
                    config.grep_terms.push(value.to_ascii_lowercase());
                }
            }
            "--priority" | "-p" => {
                let value = get_next_value(args, &mut i, "--priority")?;
                config.priority = normalize_priority(value)?;
            }
            "--max-lines" | "-n" => {
                let value = get_next_value(args, &mut i, "--max-lines")?;
                config.max_lines = Some(parse_positive_usize(&value, "--max-lines")?);
                max_lines_explicit = true;
            }
            "--top" => {
                let value = get_next_value(args, &mut i, "--top")?;
                config.top = parse_positive_usize(&value, "--top")?;
            }
            "--boot" | "-b" => {
                if has_next_boot_value(args, i) {
                    i += 1;
                    config.boot = BootFilter::Value(args[i].clone());
                } else {
                    config.boot = BootFilter::Current;
                }
            }
            _ => {
                if let Some(value) = arg.strip_prefix("--since=") {
                    config.since = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--until=") {
                    config.until = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--unit=") {
                    config.units.push(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--grep=") {
                    if !value.is_empty() {
                        config.grep_terms.push(value.to_ascii_lowercase());
                    }
                } else if let Some(value) = arg.strip_prefix("--priority=") {
                    config.priority = normalize_priority(value.to_string())?;
                } else if let Some(value) = arg.strip_prefix("--max-lines=") {
                    config.max_lines = Some(parse_positive_usize(value, "--max-lines")?);
                    max_lines_explicit = true;
                } else if let Some(value) = arg.strip_prefix("--top=") {
                    config.top = parse_positive_usize(value, "--top")?;
                } else if let Some(value) = arg.strip_prefix("--boot=") {
                    if value.is_empty() {
                        config.boot = BootFilter::Current;
                    } else {
                        config.boot = BootFilter::Value(value.to_string());
                    }
                } else {
                    return Err(format!(
                        "未知选项：{arg}\n修复：运行 logtool --help 查看可用参数。\n\n{}",
                        help_text()
                    ));
                }
            }
        }

        i += 1;
    }

    // 流模式跟随输出在未显式指定 --max-lines 时默认不截断。
    if config.mode == RunMode::Stream && config.follow && !max_lines_explicit {
        config.max_lines = None;
    }

    validate_config(&config)?;
    Ok(Action::Run(config))
}

fn standalone_action(args: &[String], arg: &str, action: Action) -> Result<Action, String> {
    if args.len() != 1 {
        return Err(format!("{arg} 不能与其他参数同时使用"));
    }
    Ok(action)
}

pub fn validate_config(config: &Config) -> Result<(), String> {
    if config.follow && config.mode == RunMode::Analyze {
        return Err(
            "--follow 只能搭配 --stream 使用\n修复：运行 logtool --stream --follow".to_string(),
        );
    }

    if config.output_json && config.mode == RunMode::Analyze {
        return Err(
            "--json 只能搭配 --stream 使用\n修复：运行 logtool --stream --json".to_string(),
        );
    }

    Ok(())
}

fn get_next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    if *index + 1 >= args.len() {
        return Err(format!(
            "缺少 {flag} 的参数值\n修复：运行 logtool --help 查看参数示例"
        ));
    }
    *index += 1;
    Ok(args[*index].clone())
}

fn has_next_boot_value(args: &[String], index: usize) -> bool {
    if index + 1 >= args.len() {
        return false;
    }

    let next = &args[index + 1];
    if !next.starts_with('-') {
        return true;
    }

    is_boot_offset(next)
}

fn is_boot_offset(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit())
}

fn parse_positive_usize(value: &str, flag: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{flag} 需要一个正整数，实际输入：{value}\n修复：示例 {flag} 50"))?;
    if parsed == 0 {
        return Err(format!("{flag} 必须大于 0\n修复：示例 {flag} 50"));
    }
    Ok(parsed)
}

fn normalize_priority(value: String) -> Result<String, String> {
    let raw = value.trim().to_ascii_lowercase();
    let normalized = match raw.as_str() {
        "0" | "emerg" | "emergency" | "panic" => "0",
        "1" | "alert" => "1",
        "2" | "crit" | "critical" => "2",
        "3" | "err" | "error" => "3",
        "4" | "warning" | "warn" => "4",
        "5" | "notice" => "5",
        "6" | "info" | "informational" | "information" => "6",
        "7" | "debug" => "7",
        _ => {
            return Err(format!(
                "无效优先级：{value}\n修复：使用 0-7 或 err/warning/info/debug（可运行：logtool --help）"
            ));
        }
    };

    Ok(normalized.to_string())
}

// ── 日志分析核心 ─────────────────────────────────────────────

pub fn analyze_journal(config: &Config) -> Result<AnalyzeResponse, String> {
    ensure_journalctl_exists()?;

    let mut cmd = build_journalctl_command_for_analysis(config);
    if config.show_command {
        eprintln!("执行命令：{}", render_command(&cmd));
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| format!("启动 journalctl 失败：{err}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法获取 journalctl 标准输出".to_string())?;

    let reader = BufReader::new(stdout);
    let mut stats: HashMap<(SourceKind, String), SourceStats> = HashMap::new();
    let mut metrics = AnalyzeMetrics::default();

    let mut loop_error: Option<String> = None;
    for maybe_line in reader.lines() {
        let line = match maybe_line {
            Ok(line) => line,
            Err(err) => {
                loop_error = Some(io_error_to_string(err));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        metrics.lines_read += 1;
        let event = match parse_json_event(&line) {
            Ok(event) => {
                metrics.parsed_ok += 1;
                event
            }
            Err(_) => {
                metrics.parse_errors += 1;
                continue;
            }
        };

        if !event_matches_terms(&event, &config.grep_terms) {
            continue;
        }

        metrics.matched += 1;
        let (kind, source) = classify_source(&event);
        let key = (kind, source.clone());

        let entry = stats.entry(key).or_insert_with(|| SourceStats {
            kind,
            source,
            count: 0,
            worst_priority: 7,
            sample_message: String::new(),
            sample_unit: None,
            sample_exe: None,
            package: None,
        });

        entry.count += 1;

        if let Some(p) = event.priority
            && p < entry.worst_priority
        {
            entry.worst_priority = p;
        }

        if !event.message.is_empty() {
            entry.sample_message = truncate_for_display(&event.message, 180);
        }

        if entry.sample_unit.is_none() {
            entry.sample_unit = event.unit.clone();
        }

        if entry.sample_exe.is_none() {
            entry.sample_exe = event.exe.clone();
        }

        if reached_limit(metrics.matched, config.max_lines) {
            break;
        }
    }

    let reached_max_lines = reached_limit(metrics.matched, config.max_lines);
    if reached_max_lines || loop_error.is_some() {
        let _ = child.kill();
    }

    let status = child.wait().map_err(io_error_to_string)?;
    if let Some(err) = loop_error {
        return Err(err);
    }
    if !status.success() && !status_killed_by_limit(metrics.matched, config.max_lines) {
        return Err(format!("journalctl 退出状态异常：{status}"));
    }

    let mut suspects = stats.into_values().collect::<Vec<_>>();
    suspects.sort_by(compare_suspects);

    resolve_packages_for_top(&mut suspects, config.top);

    Ok(AnalyzeResponse {
        metrics,
        suspects,
        top: config.top,
    })
}

/// 流模式：边读边写，每匹配一行立即通过 writer 发送 JSON StreamLine
///
/// 这是真正的流式实现——不缓冲到内存，支持 --follow 实时输出。
/// writer 通常是 Unix Socket stream 或 stdout。
pub fn stream_journal_to_writer<W: Write>(config: &Config, mut writer: W) -> Result<(), String> {
    ensure_journalctl_exists()?;

    let mut cmd = build_journalctl_command_for_stream(config);
    if config.show_command {
        eprintln!("执行命令：{}", render_command(&cmd));
    }

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| format!("启动 journalctl 失败：{err}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法获取 journalctl 标准输出".to_string())?;

    let reader = BufReader::new(stdout);
    let mut lines_written = 0usize;
    let mut stream_error: Option<String> = None;

    for maybe_line in reader.lines() {
        let line = match maybe_line {
            Ok(line) => line,
            Err(err) => {
                stream_error = Some(io_error_to_string(err));
                break;
            }
        };
        if !matches_filters(&line, &config.grep_terms) {
            continue;
        }

        let msg = StreamLine {
            line,
            done: false,
            error: None,
        };
        if let Err(err) = write_json_line(&mut writer, &msg, "流消息") {
            stream_error = Some(err);
            break;
        }

        lines_written += 1;

        if reached_limit(lines_written, config.max_lines) {
            break;
        }
    }

    let reached_max_lines = reached_limit(lines_written, config.max_lines);
    let mut killed_by_tool = false;
    if (reached_max_lines || stream_error.is_some()) && child.kill().is_ok() {
        killed_by_tool = true;
    }

    let status = child.wait().map_err(io_error_to_string)?;
    if let Some(err) = stream_error {
        return Err(err);
    }

    if !status.success()
        && !killed_by_tool
        && !status_killed_by_limit(lines_written, config.max_lines)
    {
        return Err(format!("journalctl 退出状态异常：{status}"));
    }

    let done_msg = StreamLine {
        line: String::new(),
        done: true,
        error: None,
    };
    write_json_line(&mut writer, &done_msg, "结束标记")?;

    Ok(())
}

// ── JSON 解析 ─────────────────────────────────────────────

pub fn parse_json_event(line: &str) -> Result<JournalEvent, String> {
    let value: Value = serde_json::from_str(line).map_err(|err| err.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "日志 JSON 行不是对象".to_string())?;

    let message = field_as_string(object, "MESSAGE").unwrap_or_default();
    let priority = field_as_string(object, "PRIORITY").and_then(|p| p.parse::<u8>().ok());
    let unit = field_as_string(object, "_SYSTEMD_UNIT");
    let exe = field_as_string(object, "_EXE");
    let comm = field_as_string(object, "_COMM");
    let identifier = field_as_string(object, "SYSLOG_IDENTIFIER");

    Ok(JournalEvent {
        message,
        priority,
        unit,
        exe,
        comm,
        identifier,
    })
}

fn field_as_string(map: &Map<String, Value>, key: &str) -> Option<String> {
    let raw = map.get(key)?;
    value_to_string(raw).and_then(normalize_optional)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(arr) => decode_byte_array(arr),
        _ => None,
    }
}

fn decode_byte_array(arr: &[Value]) -> Option<String> {
    let mut bytes = Vec::with_capacity(arr.len());
    for item in arr {
        let n = item.as_u64()?;
        let byte = u8::try_from(n).ok()?;
        bytes.push(byte);
    }

    String::from_utf8(bytes).ok().and_then(normalize_optional)
}

fn normalize_optional(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

// ── 过滤与分类 ─────────────────────────────────────────────

pub fn event_matches_terms(event: &JournalEvent, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }

    let mut text = String::new();
    text.push_str(&event.message);
    if let Some(unit) = &event.unit {
        text.push(' ');
        text.push_str(unit);
    }
    if let Some(exe) = &event.exe {
        text.push(' ');
        text.push_str(exe);
    }
    if let Some(comm) = &event.comm {
        text.push(' ');
        text.push_str(comm);
    }
    if let Some(id) = &event.identifier {
        text.push(' ');
        text.push_str(id);
    }

    let lower = text.to_ascii_lowercase();
    terms.iter().all(|term| lower.contains(term))
}

pub fn classify_source(event: &JournalEvent) -> (SourceKind, String) {
    if let Some(id) = &event.identifier
        && id == "kernel"
    {
        return (SourceKind::Kernel, "kernel".to_string());
    }

    if let Some(unit) = &event.unit {
        return (SourceKind::Unit, unit.clone());
    }

    if let Some(exe) = &event.exe {
        return (SourceKind::Executable, exe.clone());
    }

    if let Some(identifier) = &event.identifier {
        return (SourceKind::Identifier, identifier.clone());
    }

    if let Some(comm) = &event.comm {
        return (SourceKind::Comm, comm.clone());
    }

    (SourceKind::Unknown, "unknown".to_string())
}

fn compare_suspects(left: &SourceStats, right: &SourceStats) -> Ordering {
    right
        .count
        .cmp(&left.count)
        .then(left.worst_priority.cmp(&right.worst_priority))
        .then_with(|| left.source.cmp(&right.source))
}

// ── 包反查 ─────────────────────────────────────────────

fn resolve_packages_for_top(suspects: &mut [SourceStats], top: usize) {
    let mut resolver = PackageResolver::new();
    let limit = suspects.len().min(top);

    for suspect in suspects.iter_mut().take(limit) {
        suspect.package = resolver.resolve(suspect);
    }
}

#[derive(Default)]
struct PackageResolver {
    dpkg_available: bool,
    systemctl_available: bool,
    path_cache: HashMap<String, Option<String>>,
    unit_cache: HashMap<String, Option<String>>,
}

impl PackageResolver {
    fn new() -> Self {
        Self {
            dpkg_available: command_exists("dpkg-query"),
            systemctl_available: command_exists("systemctl"),
            path_cache: HashMap::new(),
            unit_cache: HashMap::new(),
        }
    }

    fn resolve(&mut self, suspect: &SourceStats) -> Option<String> {
        if !self.dpkg_available {
            return None;
        }

        if let Some(exe) = &suspect.sample_exe
            && let Some(pkg) = self.package_by_path(exe)
        {
            return Some(pkg);
        }

        if suspect.kind == SourceKind::Executable
            && let Some(pkg) = self.package_by_path(&suspect.source)
        {
            return Some(pkg);
        }

        if let Some(unit) = &suspect.sample_unit {
            return self.package_by_unit(unit);
        }

        if suspect.kind == SourceKind::Unit {
            return self.package_by_unit(&suspect.source);
        }

        None
    }

    fn package_by_path(&mut self, path: &str) -> Option<String> {
        if path.is_empty() || !path.starts_with('/') {
            return None;
        }

        if let Some(cached) = self.path_cache.get(path) {
            return cached.clone();
        }

        let output = Command::new("dpkg-query")
            .arg("-S")
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        let resolved = match output {
            Ok(out) if out.status.success() => {
                parse_dpkg_search_output(&String::from_utf8_lossy(&out.stdout))
            }
            _ => None,
        };

        self.path_cache.insert(path.to_string(), resolved.clone());

        resolved
    }

    fn package_by_unit(&mut self, unit: &str) -> Option<String> {
        if !self.systemctl_available {
            return None;
        }

        if let Some(cached) = self.unit_cache.get(unit) {
            return cached.clone();
        }

        let fragment_path = Command::new("systemctl")
            .arg("show")
            .arg("--property=FragmentPath")
            .arg("--value")
            .arg(unit)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        let resolved = match fragment_path {
            Ok(out) if out.status.success() => {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if path.is_empty() {
                    None
                } else {
                    self.package_by_path(&path)
                }
            }
            _ => None,
        };

        self.unit_cache.insert(unit.to_string(), resolved.clone());
        resolved
    }
}

fn parse_dpkg_search_output(output: &str) -> Option<String> {
    let line = output.lines().find(|line| line.contains(':'))?.trim();
    let mut split = line.splitn(2, ':');
    let pkg = split.next()?.trim();
    if pkg.is_empty() {
        return None;
    }
    Some(pkg.to_string())
}

fn command_exists(command: &str) -> bool {
    let status = Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    matches!(status, Ok(exit) if exit.success())
}

// ── 中文输出格式化 ─────────────────────────────────────────────

pub fn print_analysis_report(response: &AnalyzeResponse) {
    let metrics = &response.metrics;
    let suspects = &response.suspects;
    let top = response.top;

    println!("═══════════════════════════════════════════════════════════════");
    println!("                      📋 事件摘要");
    println!("═══════════════════════════════════════════════════════════════");
    println!("  读取行数    ：{}", metrics.lines_read);
    println!("  解析成功    ：{}", metrics.parsed_ok);
    println!("  匹配条数    ：{}", metrics.matched);
    println!("  解析错误    ：{}", metrics.parse_errors);
    println!("  独立来源    ：{}", suspects.len());

    if suspects.is_empty() {
        println!();
        println!("  ✅ 当前过滤条件下未发现可疑来源。");
        println!("═══════════════════════════════════════════════════════════════");
        return;
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("                    🔍 可疑来源排行");
    println!("═══════════════════════════════════════════════════════════════");

    for (index, suspect) in suspects.iter().take(top).enumerate() {
        let label = source_label_cn(suspect.kind);
        let priority_text = priority_label_cn(suspect.worst_priority);

        println!();
        println!(
            "  {}. [{}] {} | 事件数={} | 最高严重级别={}({})",
            index + 1,
            label,
            suspect.source,
            suspect.count,
            suspect.worst_priority,
            priority_text
        );

        if let Some(pkg) = &suspect.package {
            println!("     所属包  ：{pkg}");
        } else {
            println!("     所属包  ：未知");
        }

        if let Some(exe) = &suspect.sample_exe {
            println!("     可执行文件：{exe}");
        }
        if let Some(unit) = &suspect.sample_unit {
            println!("     服务单元：{unit}");
        }

        if !suspect.sample_message.is_empty() {
            println!("     示例消息：{}", suspect.sample_message);
        }
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
}

pub fn source_label_cn(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Unit => "服务单元",
        SourceKind::Executable => "可执行文件",
        SourceKind::Identifier => "标识符",
        SourceKind::Comm => "进程名",
        SourceKind::Kernel => "内核",
        SourceKind::Unknown => "未知",
    }
}

pub fn priority_label_cn(priority: u8) -> &'static str {
    match priority {
        0 => "紧急",
        1 => "警报",
        2 => "严重",
        3 => "错误",
        4 => "警告",
        5 => "通知",
        6 => "信息",
        7 => "调试",
        _ => "未知",
    }
}

// ── journalctl 命令构建 ─────────────────────────────────────────────

fn ensure_journalctl_exists() -> Result<(), String> {
    let status = Command::new("journalctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(exit) if exit.success() => Ok(()),
        Ok(_) => Err("journalctl 存在但不可用".to_string()),
        Err(err) => Err(format!("找不到 journalctl：{err}")),
    }
}

fn build_journalctl_command_for_stream(config: &Config) -> Command {
    let mut cmd = Command::new("journalctl");
    cmd.arg("--no-pager");

    if config.follow {
        cmd.arg("--follow");
    }

    add_common_query_args(&mut cmd, config);

    if config.output_json {
        cmd.arg("--output=json");
    } else {
        cmd.arg("--output=short-iso");
    }

    cmd
}

fn build_journalctl_command_for_analysis(config: &Config) -> Command {
    let mut cmd = Command::new("journalctl");
    cmd.arg("--no-pager");
    add_common_query_args(&mut cmd, config);
    cmd.arg("--output=json");
    cmd.arg("--output-fields=PRIORITY,MESSAGE,_SYSTEMD_UNIT,_EXE,_COMM,SYSLOG_IDENTIFIER");
    cmd
}

fn add_common_query_args(cmd: &mut Command, config: &Config) {
    if config.kernel_only {
        cmd.arg("--dmesg");
    }

    if let Some(since) = &config.since {
        cmd.arg("--since").arg(since);
    }

    if let Some(until) = &config.until {
        cmd.arg("--until").arg(until);
    }

    for unit in &config.units {
        cmd.arg("--unit").arg(unit);
    }

    match &config.boot {
        BootFilter::Disabled => {}
        BootFilter::Current => {
            cmd.arg("--boot");
        }
        BootFilter::Value(value) => {
            cmd.arg("--boot").arg(value);
        }
    }

    cmd.arg(format!("--priority={}", config.priority));
}

pub fn render_command(cmd: &Command) -> String {
    let mut rendered = cmd.get_program().to_string_lossy().to_string();
    for arg in cmd.get_args() {
        rendered.push(' ');
        rendered.push_str(&shell_escape(arg.to_string_lossy().as_ref()));
    }
    rendered
}

pub fn write_json_line<W: Write, T: Serialize>(
    writer: &mut W,
    payload: &T,
    label: &str,
) -> Result<(), String> {
    let json = serde_json::to_string(payload).map_err(|e| format!("序列化{label}失败：{e}"))?;
    writer
        .write_all(json.as_bytes())
        .map_err(|e| format!("发送{label}失败：{e}"))?;
    writer
        .write_all(b"\n")
        .map_err(|e| format!("发送换行符失败：{e}"))?;
    writer.flush().map_err(|e| format!("刷新输出失败：{e}"))?;

    Ok(())
}

pub fn stream_error_line(message: String) -> StreamLine {
    StreamLine {
        line: String::new(),
        done: true,
        error: Some(message),
    }
}

pub fn daemon_error(message: String) -> ErrorResponse {
    daemon_error_with_details(message, None, None)
}

pub fn daemon_error_with_details(
    message: String,
    code: Option<&str>,
    hint: Option<String>,
) -> ErrorResponse {
    ErrorResponse {
        error: message,
        code: code.map(|v| v.to_string()),
        hint,
    }
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '+'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn io_error_to_string(err: io::Error) -> String {
    err.to_string()
}

pub fn truncate_for_display(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }

    let mut out = String::with_capacity(limit + 3);
    for (idx, ch) in text.chars().enumerate() {
        if idx >= limit {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn reached_limit(count: usize, max: Option<usize>) -> bool {
    match max {
        Some(max) => count >= max,
        None => false,
    }
}

fn status_killed_by_limit(count: usize, max: Option<usize>) -> bool {
    reached_limit(count, max)
}

fn matches_filters(line: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    let lower = line.to_ascii_lowercase();
    filters.iter().all(|term| lower.contains(term))
}

// ── 帮助文本 ─────────────────────────────────────────────

pub fn help_text() -> &'static str {
    "logtool — Ubuntu 系统异常日志诊断工具

默认模式为 --analyze（归因分析，定位可疑程序/包）。

用法：
  logtool                    进入交互模式（输入 help/doctor/boots）
  logtool [命令|选项]        单次执行模式

模式：
      --analyze             归因分析模式，排列可疑程序/服务（默认）
      --stream              原始日志流模式（直接输出日志）
      analyze               归因分析模式别名
      stream                原始日志流模式别名

命令：
  help                     显示帮助（等同 --help）
  version                  显示版本（等同 --version）
  doctor                   运行环境自检（等同 --doctor）
  boots                    列出启动周期（等同 --list-boots）
  run                      按默认分析执行（适合交互模式）

交互模式：
  exit / quit / q          退出交互模式

选项：
  -h, --help                显示此帮助信息
  -v, -V, --version         显示版本信息（需单独使用）
      --doctor              运行环境自检（需单独使用）
      --list-boots          列出启动周期（需单独使用）
  -f, --follow              持续输出新日志（仅 --stream 模式）
  -k, --kernel              仅查看内核日志（等同 journalctl --dmesg）
  -u, --unit <名称>         按 systemd 服务单元过滤（可重复）
  -g, --grep <关键词>       按关键词过滤（可重复，AND 逻辑）
  -b, --boot [id]           仅当前启动周期日志，或指定启动 ID
      --all-boots           跨所有启动周期排查（默认）
  -p, --priority <级别>     优先级过滤（支持 0-7 或 err/warning/info/debug，默认：3）
  -n, --max-lines <N>       最多扫描/输出的匹配日志行数（--stream --follow 默认不限制）
      --top <N>             分析报告展示前 N 个可疑来源（默认：10）
      --since <时间>        开始时间（默认：\"2 hours ago\"）
      --until <时间>        结束时间
      --no-default-since    禁用默认时间窗口
      --json                JSON 输出（仅 --stream 模式）
      --show-command        显示生成的 journalctl 命令

示例：
  logtool
  logtool doctor
  logtool boots
  logtool --since \"30 min ago\" --top 15
  logtool --kernel --priority 4 --grep hang
  logtool --stream --follow --unit ssh
"
}

// ── 单元测试 ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[&str]) -> Result<Action, String> {
        let args = input.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        parse_args(&args)
    }

    #[test]
    fn default_mode_is_analyze() {
        let action = parse(&[]).expect("解析应成功");
        let Action::Run(config) = action else {
            panic!("应为 Action::Run");
        };

        assert_eq!(config.mode, RunMode::Analyze);
        assert_eq!(config.boot, BootFilter::Disabled);
        assert_eq!(config.since, Some(DEFAULT_SINCE.to_string()));
    }

    #[test]
    fn stream_mode_allows_follow() {
        let action = parse(&["--stream", "--follow"]).expect("解析应成功");
        let Action::Run(config) = action else {
            panic!("应为 Action::Run");
        };
        assert_eq!(config.mode, RunMode::Stream);
        assert!(config.follow);
        assert_eq!(config.max_lines, None);
    }

    #[test]
    fn help_subcommand_works() {
        let action = parse(&["help"]).expect("解析应成功");
        assert_eq!(action, Action::Help);
    }

    #[test]
    fn version_flag_returns_version_action() {
        let action = parse(&["--version"]).expect("解析应成功");
        assert_eq!(action, Action::Version);
    }

    #[test]
    fn version_short_flag_lowercase_returns_version_action() {
        let action = parse(&["-v"]).expect("解析应成功");
        assert_eq!(action, Action::Version);
    }

    #[test]
    fn doctor_command_returns_doctor_action() {
        let action = parse(&["doctor"]).expect("解析应成功");
        assert_eq!(action, Action::Doctor);
    }

    #[test]
    fn list_boots_flag_returns_action() {
        let action = parse(&["--list-boots"]).expect("解析应成功");
        assert_eq!(action, Action::ListBoots);
    }

    #[test]
    fn doctor_rejects_mixed_arguments() {
        let err = parse(&["--doctor", "--stream"]).expect_err("解析应失败");
        assert!(err.contains("--doctor"));
    }

    #[test]
    fn version_rejects_mixed_arguments() {
        let err = parse(&["--version", "--stream"]).expect_err("解析应失败");
        assert!(err.contains("--version"));
    }

    #[test]
    fn all_boots_disables_boot_filter() {
        let action = parse(&["--all-boots"]).expect("解析应成功");
        let Action::Run(config) = action else {
            panic!("应为 Action::Run");
        };
        assert_eq!(config.boot, BootFilter::Disabled);
    }

    #[test]
    fn boot_accepts_negative_offset() {
        let action = parse(&["--boot", "-1"]).expect("解析应成功");
        let Action::Run(config) = action else {
            panic!("应为 Action::Run");
        };
        assert_eq!(config.boot, BootFilter::Value("-1".to_string()));
    }

    #[test]
    fn analyze_mode_rejects_follow() {
        let err = parse(&["--follow"]).expect_err("解析应失败");
        assert!(err.contains("--follow"));
    }

    #[test]
    fn top_must_be_positive() {
        let err = parse(&["--top", "0"]).expect_err("解析应失败");
        assert!(err.contains("--top"));
    }

    #[test]
    fn priority_alias_warning_normalizes_to_numeric() {
        let action = parse(&["--priority", "warning"]).expect("解析应成功");
        let Action::Run(config) = action else {
            panic!("应为 Action::Run");
        };
        assert_eq!(config.priority, "4");
    }

    #[test]
    fn priority_invalid_value_is_rejected() {
        let err = parse(&["--priority", "verbose"]).expect_err("解析应失败");
        assert!(err.contains("无效优先级"));
    }

    #[test]
    fn stream_follow_honors_explicit_max_lines() {
        let action = parse(&["--stream", "--follow", "--max-lines", "20"]).expect("解析应成功");
        let Action::Run(config) = action else {
            panic!("应为 Action::Run");
        };
        assert_eq!(config.max_lines, Some(20));
    }

    #[test]
    fn parses_json_event() {
        let line = r#"{"MESSAGE":"segfault at 0 ip ...","PRIORITY":"3","_SYSTEMD_UNIT":"foo.service","_EXE":"/usr/bin/foo","_COMM":"foo","SYSLOG_IDENTIFIER":"foo"}"#;
        let event = parse_json_event(line).expect("JSON 应解析成功");

        assert_eq!(event.message, "segfault at 0 ip ...");
        assert_eq!(event.priority, Some(3));
        assert_eq!(event.unit.as_deref(), Some("foo.service"));
        assert_eq!(event.exe.as_deref(), Some("/usr/bin/foo"));
        assert_eq!(event.identifier.as_deref(), Some("foo"));
    }

    #[test]
    fn classify_prefers_kernel_identifier() {
        let event = JournalEvent {
            message: String::new(),
            priority: Some(3),
            unit: Some("x.service".to_string()),
            exe: Some("/usr/bin/x".to_string()),
            comm: Some("x".to_string()),
            identifier: Some("kernel".to_string()),
        };

        let (kind, source) = classify_source(&event);
        assert_eq!(kind, SourceKind::Kernel);
        assert_eq!(source, "kernel");
    }

    #[test]
    fn parses_dpkg_output() {
        let out = "openssh-server: /lib/systemd/system/ssh.service\n";
        let pkg = parse_dpkg_search_output(out);
        assert_eq!(pkg.as_deref(), Some("openssh-server"));
    }

    #[test]
    fn grep_terms_are_lowercased() {
        let action = parse(&["--grep", "FaIled"]).expect("解析应成功");
        let Action::Run(config) = action else {
            panic!("应为 Action::Run");
        };
        assert_eq!(config.grep_terms, vec!["failed".to_string()]);
    }

    #[test]
    fn stream_line_error_field_defaults_to_none() {
        let line = r#"{"line":"abc","done":false}"#;
        let parsed: StreamLine = serde_json::from_str(line).expect("JSON 应解析成功");
        assert_eq!(parsed.error, None);
    }

    #[test]
    fn daemon_error_response_serializes() {
        let payload = daemon_error("bad request".to_string());
        let json = serde_json::to_string(&payload).expect("序列化应成功");
        assert!(json.contains("\"error\":\"bad request\""));
        assert!(!json.contains("\"code\":"));
    }

    #[test]
    fn error_response_deserializes_legacy_payload() {
        let payload = r#"{"error":"old style"}"#;
        let parsed: ErrorResponse = serde_json::from_str(payload).expect("反序列化应成功");
        assert_eq!(parsed.error, "old style");
        assert_eq!(parsed.code, None);
        assert_eq!(parsed.hint, None);
    }

    #[test]
    fn daemon_error_with_details_serializes_code_and_hint() {
        let payload = daemon_error_with_details(
            "bad request".to_string(),
            Some("invalid_json"),
            Some("运行：logtool --help".to_string()),
        );
        let json = serde_json::to_string(&payload).expect("序列化应成功");
        assert!(json.contains("\"code\":\"invalid_json\""));
        assert!(json.contains("\"hint\":\"运行：logtool --help\""));
    }
}
