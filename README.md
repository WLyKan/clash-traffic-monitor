# Clash 流量监控

Windows 本地托盘工具，记录经过 Clash 的代理流量。可以按日期查看每小时的用量，选择某个小时查看各应用的下载、上传和合计排行。

## 快速开始

1. 启动 Clash Verge，再用同一 Windows 用户双击 `ClashTrafficMonitor.exe`。工具默认通过本机 Mihomo 命名管道连接。
2. 在窗口中选择日期，再点击趋势图的小时柱形，或在应用排行旁选择小时。趋势图也支持用方向键切换小时。
3. 用图表上方的「24 小时」「最近 7 天」「最近 30 天」「本周」「本月」按钮切换趋势，查看每日用量和范围累计应用排行。「本周」从本机日期的周一算起，「本月」从 1 日算起，都累计至当前。点击日期柱形可打开该日的小时趋势，再选小时查看应用。趋势图按整列响应鼠标悬停，显示完整日期或小时时段及代理合计、下载、上传，区分未采集、零流量和未来小时；静止悬停随两秒刷新更新。
4. 关闭窗口后，工具继续在通知区域采集。右键托盘图标可重新打开窗口或退出。图标可能收在 Windows 的隐藏图标区。

在「设置」可开启当前 Windows 用户登录后自动启动并收起到托盘。首次安装和从旧版升级时默认关闭。登录启动命令显式携带默认数据目录。使用独立测试目录时不提供登录自启设置，显式指定默认目录仍可使用该开关。

选择小时后，用排行旁的「整小时」下拉框查看该小时内任一 5 分钟区间的覆盖、流量和应用排行。明细滚动保留最近 14 天，按 5 分钟桶清理；已有 5 分钟历史在迁移时保留，较早版本仅有小时记录的时段不补造明细。连续采样的相邻桶可以冲销内核快照的有符号差额，未确认出口单独统计。

在「设置」可开启代理流量突增提醒并保存 MiB 阈值，默认为关闭。仅在最近 15 分钟连续采集时检测，按完全落在窗口内的采样差额累计已确认代理量。提醒包含本机时段、用量和主要贡献应用。点击通知或从托盘选择「查看最近提醒」，可打开窗口内代理量最高的 5 分钟明细。两次提醒间隔至少 15 分钟。

历史从首次成功采集起算。退出工具会停止采集，重新打开后会继续写入原数据库。

## 数据与统计口径

数据库位于 `%USERPROFILE%\Documents\ClashTrafficMonitor\traffic.sqlite3`。目录位于 AppData 外，同一 Windows 用户从 Codex、开始菜单和登录自启启动时使用相同的物理位置。按 Windows 本地日期和小时汇总，保存流量字节、应用名、访问目标域名或 IP 及采集状态。默认采样间隔为一秒，跨小时的增量按采样时间归入对应小时。

| 指标 | 含义 |
| --- | --- |
| 代理合计 | 实际出口确认走代理的上传与下载。直连从趋势和排行中剔除。 |
| 未知应用 | 已确认走代理，但无法识别进程。优先使用 Mihomo 提供的进程信息，缺失时尝试用 Windows TCP 四元组匹配。 |
| 未归因流量 | 未能对应到活跃连接的内核计数，或无法判断出口的连接字节。可能含直连或采样间隙里的短连接，单独展示。 |
| 采集覆盖 | 相邻有效快照之间实际连续采样的分钟数，跨时段时按边界拆分。首个快照仅建立基线。旧记录的精确覆盖未知。 |
| 采集缺口 | 上次采样后断连、休眠或核心重启造成的中断。离线时展示已发生的缺口，恢复后记录到数据库。 |

用本工具排查本机代理流量的来源。套餐消耗和余额以机场账户数据为准。

## 启动参数与接入

| 参数 | 用途 |
| --- | --- |
| `--start-hidden` | 启动后收在通知区域。 |
| `--data-dir <目录>` | 指定独立的数据目录，适合本地开发和测试。 |
| `--check` | 只读检查 Mihomo 连接，输出活跃连接、代理连接和已识别应用的数量。 |
| `--diagnostics` | 只读输出实际数据路径、主库中的各日汇总、采样检查点和登录启动命令。存在尚未合入主库的 WAL 时会标出，汇总可能落后于最新提交。 |

默认接入 Clash Verge。使用独立 Mihomo 时，可以设置 `CLASH_MONITOR_CONTROLLER` 和 `CLASH_MONITOR_SECRET`，控制地址必须是带端口的本机回环 HTTP 地址，并配置访问密钥。工具只调用只读接口。HTTP 接入通过断连或累计值回落识别核心重启。

如果提示无法连接，先检查 Clash Verge 是否已启动、两个程序是否由同一 Windows 用户运行。如果已连接但用量为零，检查该时段是否有实际走代理的流量。首次采样会建立基线，后续采样才记录增量。

## 合并旧历史

旧版本在 Codex 的 MSIX 环境和普通用户环境中，默认 AppData 路径可能对应不同的物理数据库。正常升级直接保留兼容库，不重新执行全量恢复。`tools/merge_history.py` 是开发期 Python 离线维护工具，不是新版发行运行时；需要恢复旧快照时，先退出所有采集实例，使用 SQLite 的 `backup` API 分别生成一致性快照，再在离线目标目录操作。需要保留的提醒设置所在快照放在前面。

工具接受重复的 `--source <冻结快照>` 和对应的 `--source-key <原数据库物理路径>`，以及 `--target <统一数据库>`。两份历史的采集时间必须不重叠；同一个小时或 5 分钟桶内的不同采集时间可以合并。工具直接累加已存汇总，保留有符号差额、检查点和旧记录覆盖标记，并用同一事务保存导入记录。重复导入相同来源与内容会跳过，同一来源内容变化时会拒绝再次全量累加。

恢复后用 `--diagnostics` 对照物理数据路径和逐日上传、下载总量，再启动采集。旧快照保留作回退依据。

## 本地开发

当前桌面源码使用 Tauri 2、React、TypeScript、Vite、Tailwind CSS、ECharts 和 Rust，Windows 优先，保留三端适配边界。Rust 核心、SQLite 仓储、后台运行时与桌面壳分别位于 `crates/monitor-core`、`crates/monitor-store`、`crates/monitor-runtime` 和 `src-tauri`；前端位于 `src`。源码集成与正式安装交付的证据范围见 [迁移记录](docs/tauri-migration.md)。

Windows 开发机需要 Rust MSVC 工具链、Visual Studio C++ Build Tools 的桌面 C++ 工作负载（MSVC 和 Windows SDK）、WebView2 Runtime，以及满足 `package-lock.json` 全部 `engines` 要求的 Node.js/npm，例如 Node.js 24.15 或更新的兼容版本。发布脚本需要 PowerShell 7。以下命令均从本仓库根目录执行。

普通依赖目录使用锁文件安装。若现有 `node_modules` 是 junction，不执行 `npm ci` 覆盖链接；先使用下面的只读校验，再在发布时传 `-SkipDependencyInstall`。

```powershell
npm ci --include=dev
```

```powershell
node tools/verify_locked_dependencies.mjs .
```

校验器按锁文件中的安装位置检查实际版本，拒绝额外包或必需包缺失，允许平台可选包未安装。常规验证命令：

```powershell
cargo test --workspace --locked
npm test
npm run build
node --test tools/verify_locked_dependencies.test.mjs
git diff --check
```

浏览器交互回归使用锁定的 Playwright；只验证前端行为，不能替代真实 Tauri IPC、托盘及安装验收。

```powershell
npm exec -- playwright install chromium
npm run test:ui
```

`npm run dev` 启动 `http://127.0.0.1:1420`；浏览器预览须显式访问 `http://127.0.0.1:1420/?demo=1`，页面会标明演示数据，不连接真实控制器或数据库。真实原生验收使用内嵌资源 EXE，并显式传入新的独立 `--data-dir`。可设置 `CARGO_TARGET_DIR`、`CLASH_FRONTEND_DIST`、`CLASH_UI_ARTIFACTS` 和 `PLAYWRIGHT_BROWSERS_PATH`，将构建、截图及浏览器缓存放到空间充足的磁盘。

固定 Python 行为夹具已入库，常规 Rust 测试无需 Python。原 Python 源码及下列命令仅保留作开发期行为对照，不作为新版发行 runtime 或 sidecar。

### Python 开发期对照

需要 Python 3.13；使用独立虚拟环境和数据库：

```powershell
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt
.\.venv\Scripts\python.exe -B -m clash_monitor --data-dir .\build\dev-data
```

原版对照验证：控制接口检查需要运行中的 Clash Verge，Tk/托盘检查需要交互桌面，均与新版测试实例隔离。

```powershell
.\.venv\Scripts\python.exe -B -m unittest discover -s tests -v
.\.venv\Scripts\python.exe -B .\tools\verify_dashboard.py --tray
.\.venv\Scripts\python.exe -B -m clash_monitor --check
```

原版真实代理/直连对照使用 `tools/verify_mihomo.py`；Rust 链路使用 `tools/verify_rust_mihomo.py`，具体命令见迁移记录。两者使用隔离 Mihomo 和本机回环流量；内核路径通过 `--core <路径>` 指定。[原版验证记录](docs/verification.md)

## 打包

正式 Windows 发行使用 Tauri/Rust。先提交源码并保持当前分支干净，再运行：

```powershell
pwsh -File tools/build-tauri.ps1 -OutputRoot D:\Releases\ClashTrafficMonitor -TargetDir D:\BuildCache\clash-target
```

`-OutputRoot` 为发布根目录，`-TargetDir` 为可复用的 Rust 构建缓存；省略后者时使用输出根下的 `cargo-target`。已有经过校验的依赖、尤其是 `node_modules` junction 时，在命令末尾增加 `-SkipDependencyInstall`；脚本仍执行 `npm ls` 和锁定版本校验。

脚本在全新子目录构建前端，以路径数组强制将资源嵌入 EXE，调用带 `--locked` 的 Tauri NSIS 构建并关闭二进制补丁，输出独立 EXE、NSIS 安装包、同一 HEAD 的源码 ZIP、`release.json`、`build-info.json` 和构建日志。清单记录提交、锁文件、工具版本和 SHA-256；诊断必须确认干净提交及实际内嵌 HTML/JavaScript。Windows 发行 runtime 不包含 Python。

构建不会自动安装、替换开始菜单或修改登录启动项。生成物仍须使用独立数据目录验收，随后完成真实入口备份、安装、历史对账和三条启动入口核验，才能视为正式交付。旧 `tools/build.ps1` 仅保留作 Python 版本的开发期对照，不用于新版发布。

## 应用访问目标

选中应用后，「访问目标排行」显示目标下载、上传和合计，按合计降序；支持所选日期整日、小时及 5 分钟时段。优先使用 host 有效域名，其次 sniffHost，再使用目标 IP，否则显示“目标未知”；IPv6 加方括号显示。域名统一小写并去除末尾点。

域名从本版启用后积累，原有字节标为“历史未记录域名”。升级只新增目标汇总表，保留原小时、应用、5 分钟、设置、恢复账本和检查点。5 分钟目标明细同样保留 14 天；采样基线、代理确认及缺口口径不变。
