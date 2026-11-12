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

本功能本地提交：`d968206`（`feat(storage): 兼容迁移 SQLite 历史与目标统计`）。

## 后台运行时与查询边界

`monitor-runtime` 持有同一物理目录的写锁，由独立采集线程一秒采样、串行提交 SQLite 和设置；UI 查询通过有限 DTO，字节使用十进制字符串。离线保留历史查询能力，目标五分钟查询与应用明细使用相同14天有效窗口。数据目录、原命令行参数和注册表登录命令保持兼容。

`collector.lock` 与旧 Python 首字节锁双向互斥。每个数据目录独立的回环端口只接受随机令牌后的“显示窗口”请求，不提供数据读写接口；目录不同的测试实例不互相拦截。只读 `--diagnostics` 不创建目录、不迁移库，明确标注 immutable 主库快照可能不包含未合并 WAL；安装备份另走 SQLite backup API。

2026-11-13 `cargo test -p monitor-runtime -- --nocapture`：17 项通过（10 单测、5 DTO/隔离 SQLite、2 原版 Python 跨进程写锁）。显式设置 `CLASH_LEGACY_PYTHON` 后实际验证 `msvcrt ↔ fs2` 双向互斥、释放后重取、不同目录并行及 junction 同物理目录互斥；临时 junction 和目录均清理。其它回归覆盖未来筛选、未来五分钟、大整数字符串、日行省略 `hour`、设置未变化不重置提醒窗口、小时选择键与前端一致、CLI 缺少目录拒绝及只读旧库诊断。

真实桌面壳、自启开关与最终默认入口仍待后续 Windows 验收；本阶段不据命令行参数单测宣称系统入口已更新。

本功能本地提交：`fccf398`（`feat(runtime): 接入后台采集与兼容启动保护`）。

## React 面板与交互

前端使用 React / TypeScript / Tailwind / ECharts，中文界面与设计变量见 `PRODUCT.md`、`DESIGN.md`。所有正式数据仅通过类型化 Tauri 命令；开发浏览器需显式 `?demo=1` 才显示带标识的合成数据，生产 bundle 不包含该演示实现。

五种趋势以整列透明操作层覆盖绘图区，空柱与短柱也能悬停/点击。提示从当前列键与最新查询数据派生，静止刷新更新、边缘避让、移出和切换清除。小时键盘选择跟随外部下拉，日趋势保留独立焦点与回车下钻。应用、目标、小时、五分钟、整日范围和提醒深链保持上下文；旧请求响应不覆盖新筛选。原生 `data_updated` 提示与两秒轮询合并，单一面板请求串行且两秒内不重复。

2026-11-13：`npm test` 17 项、`npm run build` 的类型检查与生产构建、`npm run test:ui` 2 项真实 Chromium 回归通过，覆盖整列悬停/静止刷新、日小时目标联动、设置草稿、原生提醒深链、刷新竞态和宽窄窗口。组件回归另证明“先选9时→下拉13时→右方向键14时”及多日视图不展示占位未归因0 B。浏览器通过仅证明前端，真实 Tauri IPC 和 Windows 托盘/安装门禁仍未完成。

本地前端验证：先 `npm ci`，再 `npm test`、`npm run build`、`npx playwright install chromium`、`npm run test:ui`。可通过 `CLASH_FRONTEND_DIST`、`CLASH_UI_ARTIFACTS`、`PLAYWRIGHT_BROWSERS_PATH` 将生成物与浏览器缓存定向到独立磁盘目录；数据库始终由 Rust 管理。

本功能本地提交：`4b1fa6a`（`feat(ui): 迁移趋势与流量排行至 React 面板`）。

## 真实隔离 Mihomo 与物理目录

开发期 console probe `verify_mihomo_runtime` 使用实际 Rust `MihomoSource → SamplingState → Store`，与桌面后端复用同一实现。`tools/verify_rust_mihomo.py` 只产生回环流量与保存对照证据；它不改现用内核、系统代理或 DNS。probe 要求显式认证本机控制器、新的 D 盘任务目录并拒绝复用现有库，不属于最终桌面安装包。

```powershell
cargo build -p monitor-runtime --bin verify_mihomo_runtime
python -B tools/verify_rust_mihomo.py --reader-exe <target目录>/debug/verify_mihomo_runtime.exe --core <Mihomo路径>
```

2026-11-13，真实 Mihomo v1.19.32 的 `always` 与 `off` 两轮均通过：直连阶段仅检查点增长，应用代理量保持空；两个代理客户端上/下行持续增长。`off` 原始进程字段为空，Rust 仍通过真实 Windows TCP 四元组正确识别 curl/python。数据库的小时、5 分钟、应用、IP目标总量与逐桶守恒、完整性均通过。最终回归由主代理重跑，两轮分别66/50个样本，所有本次内核/reader子进程退出，配置中的临时密钥已脱敏。

原命名管道与 SID 身份另由核心原生测试验证。真实当前 Windows 用户下的 Rust `--diagnostics` 已只读确认默认物理库仍为 `Documents\ClashTrafficMonitor\traffic.sqlite3`，4个历史日期可读；自定义目录及 junction 的物理同库写锁双向验证见运行时章节。开始菜单和登录命令的最终更新、实际运行验收仍待进行。

## 尚待完成的交付门禁

OpenSpec `openspec/changes/migrate-tauri-rust/tasks.md` 记录当前已验证项，其余任务保持未勾选。SQLite、React 和桌面运行时按独立功能验证提交；最终还须完成真实 WebView、打包 EXE / NSIS、隔离采集与托盘退出、真实入口备份安装、历史/设置/账本对账和三条启动入口物理目录验收。编译与模拟界面通过不代表安装交付完成。

数据目录始终为 `%USERPROFILE%\Documents\ClashTrafficMonitor`，保留 `--data-dir`。升级前使用 SQLite backup API 保存含 WAL 的一致性快照并备份实际快捷方式、自启配置和旧程序；回退优先让旧版接管兼容库，不覆盖为旧快照丢失新版期间新增流量。
