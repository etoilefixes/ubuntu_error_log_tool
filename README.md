# logtool

轻量级 Ubuntu 系统异常日志诊断工具（Rust）。

Lightweight Ubuntu system error-log diagnosis tool built with Rust.

## 中文文档

### 项目简介

`logtool` 由 CLI（`logtool`）和守护进程（`logtool-daemon`）组成，用于在系统卡死、异常报错、驱动故障或服务崩溃后，快速定位可疑程序、服务单元和关联软件包。

### 核心特性

- 低资源占用：守护进程常驻内存小，按需处理请求
- 异常归因：按错误频次和严重级别聚合可疑来源
- 包名反查：自动映射可执行文件到 Debian/Ubuntu 包
- 实时流式：`--stream --follow` 持续输出新日志
- systemd 集成：支持 service 管理和开机自启
- 安全访问：Unix Socket 权限 `0660`，支持专用用户组

### 架构

```text
logtool (CLI) --> Unix Socket (/run/logtool.sock) --> logtool-daemon --> journalctl
```

### 编译

```bash
cargo build --release
```

编译产物：

- `target/release/logtool`
- `target/release/logtool-daemon`

### 安装（手动）

```bash
sudo cp target/release/logtool /usr/local/bin/
sudo cp target/release/logtool-daemon /usr/local/bin/
sudo groupadd -f logtool
sudo cp logtool.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now logtool
```

### 安装（Deb）

```bash
sudo apt install ./Packages/logtool_0.2.4_amd64.deb
```

### 常用命令

```bash
# 进入交互模式
logtool

# 交互模式常用命令
# logtool> help
# logtool> doctor
# logtool> boots
# logtool> analyze --since "12 hours ago" --priority 4
# logtool> exit

# 单次执行默认分析（不进入交互）
logtool run

# 环境自检（权限 / socket / journald 持久化）
logtool doctor

# 列出启动周期（便于定位重启前后日志）
logtool boots

# 扩大范围并包含警告
logtool --priority 4 --since "12 hours ago" --top 20

# 仅内核异常（驱动/IO/挂起线索）
logtool --kernel --priority 4 --since "6 hours ago"

# 实时流式输出日志
logtool --stream --follow
```

### 权限说明

- 守护进程通常以 root 运行
- Socket：`/run/logtool.sock`，默认权限 `srw-rw---- root:logtool`
- 普通用户需加入 `logtool` 组：

```bash
sudo usermod -aG logtool $USER
newgrp logtool
sudo systemctl restart logtool
```

### 重启后日志保留（推荐）

如未启用 journald 持久化，系统重启后历史日志可能丢失。

```bash
sudo mkdir -p /var/log/journal
sudo sed -i 's/^#\?Storage=.*/Storage=persistent/' /etc/systemd/journald.conf
sudo systemctl restart systemd-journald
```

### 参数总览

| 参数 | 说明 |
|------|------|
| `run` | 单次执行默认分析（不进入交互） |
| `analyze` | `--analyze` 别名 |
| `stream` | `--stream` 别名 |
| `exit` / `quit` / `q` | 仅交互模式：退出 |
| `-h, --help` / `help` | 显示帮助信息 |
| `-V, --version` / `version` | 显示版本信息（需单独使用） |
| `--doctor` / `doctor` | 运行环境自检（需单独使用） |
| `--list-boots` / `boots` | 列出启动周期（需单独使用） |
| `--analyze` | 归因分析模式（默认） |
| `--stream` | 原始日志流模式 |
| `--since <时间>` | 开始时间（默认 `2 hours ago`） |
| `--until <时间>` | 结束时间 |
| `--boot [id]` | 仅当前启动周期或指定启动 ID |
| `--all-boots` | 跨所有启动周期排查（默认） |
| `-p, --priority <级别>` | 优先级过滤（默认 `3`） |
| `-u, --unit <名称>` | 按服务单元过滤（可重复） |
| `-k, --kernel` | 仅查看内核日志 |
| `-g, --grep <关键词>` | 关键词过滤（可重复，AND） |
| `-n, --max-lines <N>` | 最多扫描行数 |
| `--top <N>` | 展示前 N 个可疑来源（默认 `10`） |
| `--show-command` | 显示生成的 journalctl 命令 |
| `-f, --follow` | 持续输出新日志（仅 `--stream`） |
| `--json` | JSON 输出（仅 `--stream`） |

### 服务管理

```bash
sudo systemctl status logtool
sudo systemctl restart logtool
sudo journalctl -u logtool -f
```

### GitHub About 建议配置

- Description: `Lightweight Ubuntu system error log diagnosis tool in Rust.`
- Website: `https://github.com/etoilefixes/ubuntu_error_log_tool/releases`
- Topics: `rust`, `ubuntu`, `linux`, `journald`, `systemd`, `troubleshooting`, `log-analysis`

## English Documentation

### Overview

`logtool` includes a CLI (`logtool`) and a daemon (`logtool-daemon`) to help locate suspicious services, executables, and packages after system freezes, crashes, or runtime errors.

### Key Features

- Lightweight runtime footprint
- Error-source ranking by frequency and severity
- Package mapping via Debian/Ubuntu package metadata
- Real-time streaming with `--stream --follow`
- systemd service integration
- Socket-based access control (`0660`)

### Architecture

```text
logtool (CLI) --> Unix Socket (/run/logtool.sock) --> logtool-daemon --> journalctl
```

### Build

```bash
cargo build --release
```

### Install (manual)

```bash
sudo cp target/release/logtool /usr/local/bin/
sudo cp target/release/logtool-daemon /usr/local/bin/
sudo groupadd -f logtool
sudo cp logtool.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now logtool
```

### Install (Deb)

```bash
sudo apt install ./Packages/logtool_0.2.4_amd64.deb
```

### Common Usage

```bash
logtool
logtool run
logtool doctor
logtool boots
logtool analyze --priority 4 --since "12 hours ago" --top 20
logtool --priority 4 --since "12 hours ago" --top 20
logtool --kernel --priority 4 --since "6 hours ago"
logtool stream --follow
logtool --stream --follow
```

### Permission Model

- Daemon runs as root in typical systemd deployment
- Socket path: `/run/logtool.sock`
- Recommended for non-root users:

```bash
sudo usermod -aG logtool $USER
newgrp logtool
sudo systemctl restart logtool
```

### Keep Logs After Reboot (Recommended)

If persistent journald storage is not enabled, historical logs may be lost after reboot.

```bash
sudo mkdir -p /var/log/journal
sudo sed -i 's/^#\?Storage=.*/Storage=persistent/' /etc/systemd/journald.conf
sudo systemctl restart systemd-journald
```

### Service Operations

```bash
sudo systemctl status logtool
sudo systemctl restart logtool
sudo journalctl -u logtool -f
```

## License

MIT, see `LICENSE`.
