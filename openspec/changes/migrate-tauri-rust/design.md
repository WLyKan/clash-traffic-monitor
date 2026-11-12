# Design

## Context

现有 main=ce8c32e，含共享历史修复、日小时悬停和目标排行。统一数据库位于用户 Documents/ClashTrafficMonitor。原恢复表清单不含 targets/minute_targets，不能作为升级格式转换器。

## Goals / Non-Goals

**Goals:** Windows 可用的纯 Rust/Tauri 发布；所有采集和查询口径与旧版对照；完整历史、设置、恢复账本保留；逐功能验证和提交。

**Non-Goals:** 本期不宣称 macOS/Linux 完整采集支持，不新增远程服务、云同步或抓取 HTTPS 路径。

## Decisions

- Rust core 与 Tauri 分离，Windows 命名管道、进程归因和写库锁以平台模块隔离。
- rusqlite 写连接在后台串行使用，一次采样所有维度和检查点同事务，完成后发布版本更新。
- IPC 字节值采用十进制字符串，React 不执行 SQL；查询结果携带采样时间/版本，旧请求响应不得覆盖新筛选。
- 新旧程序对同一物理目录的 collector.lock 首字节互斥，独立 data-dir 允许与生产并行。
- 数据兼容首先保留 schema，保留历史导入账本与摘要。升级通过整库接管，不再次累加历史。
- 最终已提交版本构建 NSIS 和 EXE；备份数据库及启动配置后才更新实际入口。

## Risks / Trade-offs

- 连接结束后的未观察尾量仍受原接口限制；不以重写承诺恢复不可观测流量。
- signed residual、DST、旧覆盖与 IDNA/大小写兼容通过双实现固定夹具验证。
- 真实桌面锁屏时仅暂停依赖 UI 的验收，继续独立工作，验收未通过不可标记完成。

## Migration Plan

先对照契约，后 Rust core/store/Windows；UI按明确DTO并行；再壳集成、隔离EXE、真实备份安装和对账。保持旧程序与回退资料，不能默认恢复旧快照丢失新增数据。
