# Contributing / 贡献指南

## 中文

感谢你参与 `logtool`。

### 1. 开发环境

1. 安装 Rust stable 工具链
2. 克隆仓库
3. 运行基础检查

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features
cargo fmt --check
```

### 2. 开发原则

- 变更保持小而聚焦，避免无关重构
- CLI 和 daemon 的错误信息要清晰、可定位
- 任何行为变化都应更新文档
- 保持轻量化目标，避免引入重依赖

### 3. 提交流程

1. 新建分支：`feat/...`、`fix/...`、`docs/...`
2. 完成开发并自测
3. 提交信息建议使用：

- `feat: ...`
- `fix: ...`
- `docs: ...`
- `chore: ...`

### 4. Pull Request 清单

- [ ] `cargo build` 通过
- [ ] `cargo test` 通过
- [ ] `cargo clippy --all-targets --all-features` 通过
- [ ] `cargo fmt --check` 通过
- [ ] 文档已同步更新

### 5. Issue 建议内容

提交问题时建议包含：

- Ubuntu 版本、内核版本
- `logtool --version`（如已提供）
- 复现步骤
- 期望结果与实际结果
- 脱敏后的日志片段

## English

Thanks for contributing to `logtool`.

### 1. Development Setup

1. Install Rust stable toolchain
2. Clone this repository
3. Run baseline checks

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features
cargo fmt --check
```

### 2. Engineering Principles

- Keep changes focused and small
- Ensure CLI/daemon error messages are actionable
- Update docs for any behavior change
- Preserve lightweight runtime and dependency footprint

### 3. Commit Workflow

1. Create a branch: `feat/...`, `fix/...`, `docs/...`
2. Implement and test locally
3. Use concise commit messages:

- `feat: ...`
- `fix: ...`
- `docs: ...`
- `chore: ...`

### 4. Pull Request Checklist

- [ ] `cargo build` passes
- [ ] `cargo test` passes
- [ ] `cargo clippy --all-targets --all-features` passes
- [ ] `cargo fmt --check` passes
- [ ] Documentation updated

### 5. Recommended Issue Content

Please include:

- Ubuntu version and kernel version
- `logtool --version` (if available)
- Steps to reproduce
- Expected vs actual behavior
- Sanitized log snippets
