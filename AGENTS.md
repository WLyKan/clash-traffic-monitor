# 项目维护指南

## 工作范围

- 先阅读相关源码和测试，再做最小修改。分析、查看和方案类请求保持只读。获得明确授权后再改文件或提交。
- 在工作分支开发。需要提交时按独立功能分开提交，提交前检查暂存差异。避免将数据库、密钥、构建产物或个人配置纳入版本控制。
- 保持中文界面和中文用户文档。修改命令行参数、数据位置、统计口径或验证命令时同步更新 README。

## 模块职责

| 文件 | 职责 |
| --- | --- |
| `clash_monitor/source.py` | 读取 Mihomo 快照、判断实际出口、归因 Windows 应用。 |
| `clash_monitor/collector.py` | 管理采样生命周期、计算增量、识别缺口与核心切换。 |
| `clash_monitor/storage.py` | SQLite 写入、小时汇总和应用排行。 |
| `clash_monitor/ui.py` | 日期选择、小时趋势、排行和采集状态。 |
| `clash_monitor/tray.py` | Windows 通知区域图标和窗口生命周期。 |
| `clash_monitor/__main__.py` | 命令行入口、数据目录和单写者保护。 |

## 实现约束

- 对现用 Clash 仅调用只读接口。修改采集或验证逻辑时，使用当前用户的本地命名管道，或带密钥的回环 HTTP。测试流量优先在隔离内核和回环环境产生。
- 明确区分代理、直连和未知出口。直连及未知出口都不能计入已确认的代理合计。确认走代理但进程未知时，归入「未知应用」。
- 首次采样、重连、核心重启或累计计数回落时建立新基线。保留小时内有符号差额的对账逻辑，避免相邻非原子快照重复计入未归因流量。
- 按 Windows 本地日期与小时归桶，保持应用汇总、小时记录和采样检查点在同一事务中。改表结构时兼顾已有数据库，并用独立数据目录验证升级。
- Tk 控件只在主线程更新。关闭窗口隐藏到托盘，退出托盘才停止采集。使用 `--data-dir` 将验证与正在运行的后台采集隔离。
- 数据库只保存汇总所需字段。日志、测试夹具与截图避免包含控制密钥、订阅信息、访问目标或真实流量明细。

## 验证

从项目根目录执行，依赖安装方式见 [README](README.md)。

```powershell
.\.venv\Scripts\python.exe -B -m unittest discover -s tests -v
git diff --check
```

- 改界面或托盘时，在 Windows 桌面会话运行 `.\.venv\Scripts\python.exe -B .\tools\verify_dashboard.py --tray`。
- 改采集和归因时，覆盖代理、直连、未知应用、连接结束和重连。需要实测时运行 `.\.venv\Scripts\python.exe -B .\tools\verify_mihomo.py`，或用 `--check` 只读检查现用内核。
- 改入口、依赖或打包时，执行 `.\tools\build.ps1 -Python .\.venv\Scripts\python.exe`，用独立数据目录验证生成的 EXE 能启动、采集并通过托盘退出。
