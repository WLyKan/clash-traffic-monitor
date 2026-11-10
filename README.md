# Clash 流量监控

Windows 本地托盘工具，记录经过 Clash 的代理流量。可以按日期查看每小时的用量，选择某个小时查看各应用的下载、上传和合计排行。

## 快速开始

1. 启动 Clash Verge，再用同一 Windows 用户双击 `ClashTrafficMonitor.exe`。工具默认通过本机 Mihomo 命名管道连接。
2. 在窗口中选择日期，再点击趋势图的小时柱形，或在应用排行旁选择小时。趋势图也支持用方向键切换小时。
3. 用趋势下拉框切换最近 7 天或 30 天，查看含今天的每日用量和范围累计应用排行。「本周」从本机日期的周一算起，「本月」从 1 日算起，都累计至当前。点击日期柱形可打开该日的小时趋势，再选小时查看应用。
4. 关闭窗口后，工具继续在通知区域采集。右键托盘图标可重新打开窗口或退出。图标可能收在 Windows 的隐藏图标区。

在「设置」可开启当前 Windows 用户登录后自动启动并收起到托盘。首次安装和从旧版升级时默认关闭。使用独立数据目录时不提供登录自启设置。

选择小时后，用排行旁的「整小时」下拉框查看该小时内任一 5 分钟区间的覆盖、流量和应用排行。明细滚动保留最近 14 天，按 5 分钟桶清理；升级前只有小时历史。连续采样的相邻桶可以冲销内核快照的有符号差额，未确认出口单独统计。

在「设置」可开启代理流量突增提醒并保存 MiB 阈值，默认为关闭。仅在最近 15 分钟连续采集时检测，按完全落在窗口内的采样差额累计已确认代理量。提醒包含本机时段、用量和主要贡献应用。点击通知或从托盘选择「查看最近提醒」，可打开窗口内代理量最高的 5 分钟明细。两次提醒间隔至少 15 分钟。

历史从首次成功采集起算。退出工具会停止采集，重新打开后会继续写入原数据库。

## 数据与统计口径

数据库位于 `%LOCALAPPDATA%\ClashTrafficMonitor\traffic.sqlite3`。按 Windows 本地日期和小时汇总，只保留流量字节、应用名及采集状态。默认采样间隔为一秒，跨小时的增量按采样时间归入对应小时。

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

默认接入 Clash Verge。使用独立 Mihomo 时，可以设置 `CLASH_MONITOR_CONTROLLER` 和 `CLASH_MONITOR_SECRET`，控制地址必须是带端口的本机回环 HTTP 地址，并配置访问密钥。工具只调用只读接口。HTTP 接入通过断连或累计值回落识别核心重启。

如果提示无法连接，先检查 Clash Verge 是否已启动、两个程序是否由同一 Windows 用户运行。如果已连接但用量为零，检查该时段是否有实际走代理的流量。首次采样会建立基线，后续采样才记录增量。

## 本地开发

需要 Windows 和 Python 3.13。以下命令从项目根目录执行。

```powershell
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt
.\.venv\Scripts\python.exe -B -m clash_monitor --data-dir .\build\dev-data
```

在单独的终端执行验证。控制接口检查需要运行中的 Clash Verge。界面和托盘检查需要 Windows 桌面会话，使用独立的测试数据库。

```powershell
.\.venv\Scripts\python.exe -B -m unittest discover -s tests -v
.\.venv\Scripts\python.exe -B .\tools\verify_dashboard.py --tray
.\.venv\Scripts\python.exe -B -m clash_monitor --check
```

如需验证真实代理与直连对照，运行 `tools\verify_mihomo.py`。脚本启动隔离的 Mihomo，使用本机回环流量验证。内核路径与脚本默认值不同时，使用 `--core <路径>`。[验证记录](docs/verification.md)

## 打包

```powershell
.\.venv\Scripts\python.exe -m pip install -r requirements-build.txt
.\tools\build.ps1 -Python .\.venv\Scripts\python.exe
```

在 Windows x64 上生成 `dist\ClashTrafficMonitor.exe`。可用 `--data-dir` 指定测试目录，对打包版做独立的连接和托盘验收。
