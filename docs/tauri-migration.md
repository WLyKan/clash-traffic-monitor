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

本功能本地提交：`90c816c`（`feat(core): 迁移代理采样与归因核心至 Rust`）。

## SQLite 历史兼容与恢复

`monitor-store` 保留原小时、应用、5 分钟、目标、提醒设置、检查点和 `history_imports` 表。一次采样的各维度及检查点同事务写入，覆盖按本地边界拆分；相邻桶只在同一 segment 冲销有符号残差。目标合计包含未知项，历史缺少目标的差额显示“历史未记录域名”。5 分钟数据沿用 14 天保留期，小时历史保留。

首次打开直接接管兼容 schema，不自动恢复或全量重复导入。显式恢复 API 使用原路径/摘要兼容账本，拒绝已变化的来源，重复相同来源跳过；旧 Python 已恢复来源仅补目标维度。备份使用 SQLite backup API，包含已提交 WAL。回退继续使用兼容整库，不默认用升级前备份覆盖新增记录。

2026-11-13 `cargo test -p monitor-store`：23 项通过（1 单测、11 恢复、11 仓储），包括旧库迁移、Python 查询/摘要合同、WAL 备份、重复来源/物理别名拒绝、失败事务回滚、跨日/小时/五分钟残差、明细保留、`2^53` 以上字节精度、动态缺口及 `100 → 99 → 100` 时钟回拨联测。另覆盖旧导入聚合的继承账本补齐、传递自恢复拒绝、账本冲突整批回滚、离线超过14天时应用/目标五分钟过滤一致。`cargo fmt -p monitor-store --check` 通过。测试均使用隔离数据库，无生产数据库写入。

重建开发期仓储夹具：`python -B tools/export_store_fixtures.py`；常规 Rust 回归直接读取已提交 JSON。

## 尚待完成的交付门禁

OpenSpec `openspec/changes/migrate-tauri-rust/tasks.md` 记录当前已验证项，其余任务保持未勾选。SQLite、React 和桌面运行时按独立功能验证提交；最终还须完成真实 WebView、打包 EXE / NSIS、隔离采集与托盘退出、真实入口备份安装、历史/设置/账本对账和三条启动入口物理目录验收。编译与模拟界面通过不代表安装交付完成。

数据目录始终为 `%USERPROFILE%\Documents\ClashTrafficMonitor`，保留 `--data-dir`。升级前使用 SQLite backup API 保存含 WAL 的一致性快照并备份实际快捷方式、自启配置和旧程序；回退优先让旧版接管兼容库，不覆盖为旧快照丢失新版期间新增流量。
