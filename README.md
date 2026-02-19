# logtool — Ubuntu 系统异常日志诊断工具

系统级后台服务 + CLI 命令行工具，快速定位 Ubuntu 系统卡死/崩溃/报错的可疑程序。

## 特性

- 🔍 **归因分析**：自动统计可疑程序/服务，按出错频次排行
- 📦 **包名反查**：自动关联 dpkg 包名，方便修复或回滚
- 🌊 **真正流式**：边读边发，`--stream --follow` 实时输出，低内存占用
- 🇨🇳 **全中文界面**：所有输出均为中文
- 🔧 **systemd 集成**：后台服务，随用随查
- 🧵 **多线程处理**：每个连接独立线程，互不阻塞
- 🔒 **权限控制**：Socket 权限 0660，仅 root 和同组用户可访问

## 架构

```
logtool（CLI） ──Unix Socket──▶ logtool-daemon（后台服务）──▶ journalctl
```

## 编译

```bash
cargo build --release
```

编译产物：
- `./target/release/logtool` — CLI 命令
- `./target/release/logtool-daemon` — 守护进程

## Deb 包

仓库提供预构建 Deb 包（用于发布）：

- `./Packages/logtool_<version>_<arch>.deb`

安装 Deb：

```bash
sudo apt install ./Packages/logtool_<version>_<arch>.deb
```

## 项目目录

```text
.
├── src/                    # Rust 源码（库 + CLI + daemon）
├── Packages/               # 发布用 deb 包
├── logtool.service         # 手动安装时使用的 systemd unit
├── Cargo.toml
└── README.md
```

## 安装

```bash
# 复制二进制到系统路径
sudo cp target/release/logtool /usr/local/bin/
sudo cp target/release/logtool-daemon /usr/local/bin/

# 创建专用组（允许普通用户通过 socket 访问）
sudo groupadd -f logtool

# 安装 systemd 服务
sudo cp logtool.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now logtool
```

## 使用

### 1）系统卡死/报错后，直接查"谁最可疑"

```bash
logtool
```

默认行为：
- 当前启动周期（`--boot`）
- 最近 2 小时（`--since "2 hours ago"`）
- 仅错误及以上（`--priority 3`）
- 输出可疑程序/服务排行和对应包信息

### 2）扩大范围查卡死（含警告）

```bash
logtool --priority 4 --since "12 hours ago" --top 20
```

### 3）只查内核级异常（驱动/IO/hang）

```bash
logtool --kernel --priority 4 --since "6 hours ago"
```

### 4）实时看原始错误日志

```bash
logtool --stream --follow
```

## 参数说明

| 参数 | 说明 |
|------|------|
| `--analyze` | 归因分析模式（默认） |
| `--stream` | 原始日志流模式 |
| `--since <时间>` | 开始时间（默认 "2 hours ago"） |
| `--until <时间>` | 结束时间 |
| `--boot [id]` | 当前启动周期或指定启动 ID |
| `--all-boots` | 跨所有启动周期排查 |
| `-p, --priority <级别>` | 优先级过滤（默认 3/错误） |
| `-u, --unit <名称>` | 按服务单元过滤（可重复） |
| `-k, --kernel` | 仅查看内核日志 |
| `-g, --grep <关键词>` | 关键词过滤（可重复，AND 逻辑） |
| `-n, --max-lines <N>` | 最多扫描行数 |
| `--top <N>` | 展示前 N 个可疑来源（默认 10） |
| `--show-command` | 显示生成的 journalctl 命令 |
| `-f, --follow` | 持续输出新日志（仅 --stream） |
| `--json` | JSON 输出（仅 --stream） |

## 服务管理

```bash
# 查看服务状态
sudo systemctl status logtool

# 重启服务
sudo systemctl restart logtool

# 查看服务日志
sudo journalctl -u logtool -f
```

## 权限说明

守护进程以 root 运行，Socket 权限为 `0660`（仅 owner 和同组用户可访问）。

- **root 用户**：直接使用 `sudo logtool`
- **普通用户**：建议创建专用 `logtool` 组并将用户加入该组（不要加入 root 组）

示例：

```bash
sudo groupadd -f logtool
sudo usermod -aG logtool $USER
sudo systemctl restart logtool
```
