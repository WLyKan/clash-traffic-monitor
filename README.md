# Clash 流量监控

Windows 本地托盘工具。按日期看每小时的代理流量，点击柱形查看该小时应用的下载、上传和合计排名。

## 使用

1. 确认 Clash Verge 正在运行，双击 `ClashTrafficMonitor.exe`。应用使用当前 Windows 用户的 Mihomo 命名管道自动连接。
2. 选择日期后，点击某个小时的柱形，或在应用排行旁选小时。趋势区也支持方向键选择小时。
3. 关闭窗口后，采集继续在通知区域运行。右键托盘图标可重新打开窗口或退出。如果图标折叠在 Windows 通知区域，可展开隐藏的图标查看。

数据保存在 `%LOCALAPPDATA%\ClashTrafficMonitor\traffic.sqlite3`。每次采集按 Windows 本地时间归入日期和小时，只存汇总的字节与应用名。跨小时的流量按采样时间归入，边界精度约一秒。若连续采集中断，恢复后会标记缺口。停用期间和初次启动前的流量无法补录。

## 统计口径

- **代理合计**包含实际出站节点已识别为代理的流量。直连从趋势和排行剔除。
- **未知应用**仍属于确认走代理的流量。优先使用 Mihomo 进程信息，缺失时用 Windows TCP 四元组匹配应用。
- **未归因流量**包含内核总计中无法对应到活跃连接的字节，以及出口类型无法识别的字节。这部分可能含直连，单独展示。短连接或关闭前的最后一段流量可能落在这里。
- **采集缺口**指连接控制接口、保存样本或采样连续性中断的区间。代理流量用于定位本机消耗，机场的账户用量以服务端为准。

## 本地开发

使用 Windows 和 Python 3.13。安装运行和打包依赖，运行单元测试，再生成 Windows x64 可执行文件。

```powershell
python -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements-build.txt
.\.venv\Scripts\python.exe -B -m unittest discover -s tests -v
.\.venv\Scripts\python.exe -B .\tools\verify_dashboard.py --tray
.\.venv\Scripts\python.exe -B -m clash_monitor --check
.\tools\build.ps1 -Python .\.venv\Scripts\python.exe
```

通过 `--data-dir` 指定测试数据库目录，通过 `--start-hidden` 从托盘启动。若使用独立 Mihomo，可设置 `CLASH_MONITOR_CONTROLLER` 和 `CLASH_MONITOR_SECRET`，接口必须是本机回环 HTTP，且有访问密钥。应用只使用读取接口。HTTP 接入依靠断连或累计值回落识别核心重启。实测方法见 `docs/verification.md`。
