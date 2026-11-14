# Tauri / Rust 迁移记录

本次目标为 Windows 完整交付、保留三端适配结构。最终发布采用 Tauri 2、React、TypeScript、Vite、Tailwind CSS、ECharts 和 Rust；最终运行与安装不依赖 Python 或 sidecar。原 Python 程序保留作开发期对照，不与新版同时写真实数据库。

开发来源分支：`codex/migrate-tauri-rust`，基于 `main` 的 `ce8c32e`，包含 `f163d32` 共享历史修复、`7c26f40` 悬停和 `ce8c32e` 目标排行；本次源码收口整合至 `main`。用户 `.vscode/` 与既有其他 worktree 不属于本次修改范围。

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

真实桌面界面的用户手动验收见后文；系统自启、正式安装和最终默认入口仍保留独立验收门禁，不据命令行参数单测宣称系统入口已更新。

本功能本地提交：`fccf398`（`feat(runtime): 接入后台采集与兼容启动保护`）。

## React 面板与交互

前端使用 React / TypeScript / Tailwind / ECharts，中文界面与设计变量见 `PRODUCT.md`、`DESIGN.md`。所有正式数据仅通过类型化 Tauri 命令；开发浏览器需显式 `?demo=1` 才显示带标识的合成数据，生产 bundle 不包含该演示实现。

五种趋势以整列透明操作层覆盖绘图区，空柱与短柱也能悬停/点击。提示从当前列键与最新查询数据派生，静止刷新更新、边缘避让、移出和切换清除。小时键盘选择跟随外部下拉，日趋势保留独立焦点与回车下钻。应用、目标、小时、五分钟、整日范围和提醒深链保持上下文；旧请求响应不覆盖新筛选。原生 `data_updated` 提示与两秒轮询合并，单一面板请求串行且两秒内不重复。

2026-11-13：`npm test` 17 项、`npm run build` 的类型检查与生产构建、`npm run test:ui` 2 项真实 Chromium 回归通过，覆盖整列悬停/静止刷新、日小时目标联动、设置草稿、原生提醒深链、刷新竞态和宽窄窗口。组件回归另证明“先选9时→下拉13时→右方向键14时”及多日视图不展示占位未归因0 B。浏览器测试仅证明前端；真实原生界面的用户手动确认另记于桌面壳章节，正式安装门禁仍未完成。

本地前端验证：普通依赖目录先 `npm ci --include=dev`；已有 `node_modules` junction 时保留链接并执行 `node tools/verify_locked_dependencies.mjs .`，不覆盖安装。随后执行 `npm test`、`npm run build`、`npm exec -- playwright install chromium`、`npm run test:ui`。可通过 `CLASH_FRONTEND_DIST`、`CLASH_UI_ARTIFACTS`、`PLAYWRIGHT_BROWSERS_PATH` 将生成物与浏览器缓存定向到独立磁盘目录；数据库始终由 Rust 管理。

本功能本地提交：`4b1fa6a`（`feat(ui): 迁移趋势与流量排行至 React 面板`）。

## 真实隔离 Mihomo 与物理目录

开发期 console probe `verify_mihomo_runtime` 使用实际 Rust `MihomoSource → SamplingState → Store`，与桌面后端复用同一实现。`tools/verify_rust_mihomo.py` 只产生回环流量与保存对照证据；它不改现用内核、系统代理或 DNS。probe 要求显式认证本机控制器、新的 D 盘任务目录并拒绝复用现有库，不属于最终桌面安装包。

```powershell
cargo build -p monitor-runtime --bin verify_mihomo_runtime
python -B tools/verify_rust_mihomo.py --reader-exe <target目录>/debug/verify_mihomo_runtime.exe --core <Mihomo路径>
```

2026-11-13，真实 Mihomo v1.19.32 的 `always` 与 `off` 两轮均通过：直连阶段仅检查点增长，应用代理量保持空；两个代理客户端上/下行持续增长。`off` 原始进程字段为空，Rust 仍通过真实 Windows TCP 四元组正确识别 curl/python。数据库的小时、5 分钟、应用、IP目标总量与逐桶守恒、完整性均通过。最终回归由主代理重跑，两轮分别66/50个样本，所有本次内核/reader子进程退出，配置中的临时密钥已脱敏。

原命名管道与 SID 身份另由核心原生测试验证。真实当前 Windows 用户下的 Rust `--diagnostics` 已只读确认默认物理库仍为 `Documents\ClashTrafficMonitor\traffic.sqlite3`，4个历史日期可读；自定义目录及 junction 的物理同库写锁双向验证见运行时章节。开始菜单和登录命令的最终更新、实际运行验收仍待进行。

## 真实历史冻结副本接管

`real_snapshot.rs` 是显式启用的兼容验收，必须设置 `CLASH_COMPAT_SNAPSHOT` 指向本任务 D 盘 `preflight-*` 下无 WAL 的一致性冻结副本；未设置时明确输出 SKIP，常规测试通过不能替代这项正式执行。源以 immutable 方式只读，所有迁移与采样写入新的 D 盘临时副本。

2026-11-13 正式执行该测试通过：10 张表、6241 条原行、4 个历史日期、2 条恢复账本在 `Store::open` 后逐行保留；两次合成采样之后旧日期字节、旧应用/目标/小时/五分钟记录、提醒设置和导入账本保持，检查点前进，新应用三个时段范围的目标上传/下载为11/23字节且守恒。允许中断期间增加真实 gap_seconds。源 SHA-256 前后完全一致，临时副本清理，原生产程序没有停止或更新。

另外在 Codex 沙箱工具上下文与真实当前 Windows 用户上下文分别只读核对默认路径，解析出的数据库、设备标识和文件 ID 一致，避免仅凭路径字符串判断同库。真实开始菜单/登录启动的新程序验收仍保留为最终门禁。

## Windows 桌面壳与用户手动验收

桌面壳复用 Rust 后台采集与查询，主窗口关闭后隐藏，托盘提供打开面板、查看最近流量提醒和退出。窗口的 WebView 数据目录位于所选 `--data-dir` 下。CLI 诊断的 `frontend_mode` 区分 `embedded`、`development-server`、`external-url` 和 `missing-assets`；另有 `embedded_index`、`embedded_script` 检查实际编入的 HTML 与 JavaScript。独立 EXE 验收必须使用实际内嵌资源版本，不能只凭非开发编译模式或 CLI 检查判断界面可用。

Tauri 的 `App::run` 最终直接退出进程，不能依赖托管状态的析构清理后台线程。`DesktopWorkers` 在退出事件、构建失败和 setup 失败时显式停止并等待焦点服务及提醒线程；焦点服务清理自己拥有的 `instance.json`，随后停止采集并等待事务完成。两项无 GUI 回归覆盖 AppHandle 回调形成的持有环、线程等待、端点删除、重复清理及 setup 尚未建立线程的情况。

2026-11-13 资源路径修复后完整 `cargo test --workspace --locked --offline -- --nocapture`：67 项通过，显式启用原 Python 跨进程写锁及真实历史冻结副本测试，未跳过这两项验收。Release 的 11 项只读 CLI 检查通过。独立认证回环采集验收另取得 783 个样本、小时与五分钟目标零差异、SQLite 完整性通过；受保护的 Win32 原生托盘菜单命令退出码为 0，焦点端点已删除。早期预检显示目录列表的产物未计为界面通过，也不作为正式交付产物。

原生验收定位到 Windows 外部构建目录的盘符字符串被 Tauri 优先反序列化为 URL，导致资源未编入。发行配置改用单元素路径数组，强制递归嵌入该目录；诊断同时检查实际 HTML/JS。回归先复现错误，再验证盘符 URL、路径数组、资源缺失和开发模式，连同退出清理测试通过。修复版 CLI 已确认 HTML 与 JavaScript 实际内嵌，`main` capability 及构建 ACL 覆盖刷新和提醒事件的监听。资源 guard、完整 Rust 回归及隔离生命周期验证属于自动化证据。

用户于 **2026-11-13** 回复“**好了验收通过**”，据此记录真实原生界面为**用户手动验收通过**，OpenSpec 4.3 已完成。未找到默认位置的手工验收机器报告，不将用户确认改写为自动化报告，也不据此宣称最终干净主分支 EXE/NSIS、系统自启或实际安装入口已验收。`tools/prepare_native_fixture.py` 提供独立合成历史及 `expected.json`，用于人工核对 0 B、未采集、未来小时、旧覆盖未知、日/小时/五分钟及目标联动，不访问生产数据库。

## 发布脚本（待正式构建）

`tools/build-tauri.ps1` 仅接受已提交且干净的工作分支，创建新的发布目录，以路径数组配置外部前端目录，构建内嵌前端的 EXE 与 NSIS，记录提交、锁文件、配置、工具版本及产物 SHA-256，并从同一 HEAD 导出源码。脚本使用 `--no-binary-patching`，避免安装包内 EXE 与独立交付 EXE 的字节不同。Git 状态无法读取时，构建标识保守标记为 dirty；正式构建另有首尾 Git 状态与提交核对。

复用 `node_modules` junction 时使用 `-SkipDependencyInstall`，脚本仍执行 `npm ls` 和 `verify_locked_dependencies.mjs`。后者按锁文件安装位置精确比较实际版本，拒绝额外包及必需包缺失，允许未安装的 optional 包。6 项隔离回归通过，涵盖 semver 范围内的版本差异、嵌套/作用域包、额外包、必需/可选缺失、只读校验与 Windows junction；本机实际 126 包匹配，74 个可选包未安装。可独立执行 `node --test tools/verify_locked_dependencies.test.mjs`，测试数据限于任务 D 盘目录。

桌面源码已取得上述用户手动确认；正式发布脚本仍须在最终提交且干净的 `main` 上完整执行。已有预检构建不替代该步骤，也不用于声明最终安装交付完成。

## 源码集成与正式交付边界

源码迁移以已验证的核心、仓储、后台和面板功能提交，以及本次桌面壳、发行工具与文档的中文集成提交收口至 `main`。各独立功能提交及验证对应关系见上文；本次集成提交和合并记录以 `main` 的 Git 日志为准。用户已确认“提交、合并并清理”，仅清理提交确已进入 `main` 的功能分支。

OpenSpec `openspec/changes/migrate-tauri-rust/tasks.md` 区分源码验证、用户手动界面确认和正式交付。当前已验证资源 guard、67 项 Rust、17 项 React、2 项浏览器和 6 项锁定依赖回归；正式干净主分支 EXE/NSIS 构建、该产物的隔离采集/托盘退出、真实入口备份安装、系统登录启动、历史/设置/账本最终对账和开始菜单/登录/Codex 三条入口仍未完成，相关任务保持未勾选。源码合入不代表正式安装交付完成。

数据目录始终为 `%USERPROFILE%\Documents\ClashTrafficMonitor`，保留 `--data-dir`。升级前使用 SQLite backup API 保存含 WAL 的一致性快照并备份实际快捷方式、自启配置和旧程序；回退优先让旧版接管兼容库，不覆盖为旧快照丢失新版期间新增流量。
