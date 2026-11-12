# Tauri / Rust 迁移记录

本次目标为 Windows 完整交付、保留三端适配结构。最终发布采用 Tauri 2、React、TypeScript、Vite、Tailwind CSS、ECharts 和 Rust；最终运行与安装不依赖 Python 或 sidecar。原 Python 程序保留作开发期对照，不与新版同时写真实数据库。

实现分支：`codex/migrate-tauri-rust`，基于 `main` 的 `ce8c32e`，包含 `f163d32` 共享历史修复、`7c26f40` 悬停和 `ce8c32e` 目标排行。用户 `.vscode/` 与既有其他 worktree 不属于本次修改范围。

## Rust 核心

`monitor-core` 实现差分状态机、Mihomo 只读 HTTP / Windows 命名管道、出口判断、应用与目标归因、本地时间和 15 分钟提醒。`Transport` 是平台与控制器边界；非 Windows 当前要求显式配置带密钥的本机 HTTP，未承诺其它平台具有 Windows TCP 归因能力。

统计使用 `i64` 保留正负残差。先 `prepare`，数据库事务成功后才 `commit`；首样、离线、休眠、核心切换、计数回落和时钟回拨建立新基线。时钟回拨不会生成未来缺口，也不等待墙钟追上旧检查点才恢复采样。

2026-11-13 Windows 隔离验证：

- `cargo test -p monitor-core`：21 项通过，包括 Python 固定夹具逐字段对照。
- 原生命名管道 Content-Length / chunked 响应、超时取消并排空、IPv4 / IPv6 TCP 所有者与进程创建时间、当前用户 SID 管道路径通过。
- `cargo fmt -p monitor-core --check` 通过。
- 原项目 `python -B -m unittest discover -s tests -v`：51 项通过，257.635 秒，均用隔离数据库。

重新生成开发期核心对照夹具：`python -B tools/export_core_fixtures.py`。Rust 回归消费已提交的 JSON，无需启动现用 Mihomo 或写生产数据库。

## 尚待完成的交付门禁

OpenSpec `openspec/changes/migrate-tauri-rust/tasks.md` 记录当前已验证项，其余任务保持未勾选。SQLite、React 和桌面运行时按独立功能验证提交；最终还须完成真实 WebView、打包 EXE / NSIS、隔离采集与托盘退出、真实入口备份安装、历史/设置/账本对账和三条启动入口物理目录验收。编译与模拟界面通过不代表安装交付完成。

数据目录始终为 `%USERPROFILE%\Documents\ClashTrafficMonitor`，保留 `--data-dir`。升级前使用 SQLite backup API 保存含 WAL 的一致性快照并备份实际快捷方式、自启配置和旧程序；回退优先让旧版接管兼容库，不覆盖为旧快照丢失新版期间新增流量。
