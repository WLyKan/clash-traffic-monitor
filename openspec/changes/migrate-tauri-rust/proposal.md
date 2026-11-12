# Proposal

## Why

将现有本地 Python/Tk 流量工具迁移到可持续维护的现代桌面界面，以 Rust 负责全部后台能力，保持既有流量计量口径与已恢复历史。

## What Changes

- 新增 Tauri 2 / React / TypeScript / Tailwind 界面，Windows 首期完整交付，保留平台适配边界。
- 纯 Rust 采样、SQLite、提醒和系统集成，最终运行及安装不依赖 Python。
- 兼容当前统一 Documents 数据库与 collector.lock，沿用现有日、小时、五分钟、目标历史。
- 每个独立功能验证后进行中文本地提交，最终安装和真实入口验收另行记录。

## Capabilities

### New Capabilities

- `traffic-accounting`: 差分采样、出口与应用目标归因、时间桶与提醒。
- `history-compatibility`: SQLite 历史、恢复账本、数据目录和跨版本写库锁。
- `desktop-dashboard`: 中文趋势与排行、Tauri IPC、窗口托盘与实际安装入口。

### Modified Capabilities

无现有 OpenSpec capability。本次将已有行为首次形成规格。

## Impact

新增 Rust workspace、Tauri shell 与 React frontend；原 Python 保留为开发期行为对照。生产数据必须经隔离验证后才接管。用户 .vscode 和其他分支不属于修改范围。
