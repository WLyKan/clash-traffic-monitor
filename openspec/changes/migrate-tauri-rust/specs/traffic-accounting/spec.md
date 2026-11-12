# Spec Delta

## Purpose

保持已确认代理计量、连接基线、目标归因、时间桶和覆盖质量，既有小时历史与五分钟明细在 Rust 中得到相同统计结果。

## ADDED Requirements

### Requirement: 连接增量与质量
系统 SHALL 仅对确认代理连接累计差分，首样、重连、核心变化和计数回落建立基线；未采集、零代理量、旧覆盖未知和未来时段分别表示。

#### Scenario: 首见与断连
- **WHEN** 同一固定快照序列交给 Python 与 Rust
- **THEN** 应用/目标上下行和覆盖/缺口逐字段一致，无重复计量

### Requirement: signed residual 和时间桶
系统 SHALL 保留有符号残差，仅在相同连续 segment 的相邻五分钟桶冲销；跨小时调整小时残差，按本机时间拆分覆盖。

#### Scenario: 跨边界
- **WHEN** 连续快照跨小时、五分钟或本地日期边界
- **THEN** 桶记录和原有口径一致，断连两侧不相互冲销

### Requirement: 目标与提醒
系统 SHALL 使用 host/sniffHost/IP/未知优先级，目标名变化只承接后续增量；提醒要求连续覆盖15分钟并保持阈值与冷却。

#### Scenario: 目标变化
- **WHEN** 同一连接从IP变为域名且累计流量继续增长
- **THEN** 只将新增字节归属新名称，目标和应用合计一致
