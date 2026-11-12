# Spec Delta

## Purpose

保持当前共享物理数据库中的完整历史、应用目标和导入来源账本，以可验证的锁与事务保护新旧程序接管和兼容回退。

## ADDED Requirements

### Requirement: 共享数据和互斥
系统 SHALL 沿用 Documents/ClashTrafficMonitor 与 --data-dir，相同物理目录沿用 collector.lock 首字节跨版本互斥。

#### Scenario: 双向启动
- **WHEN** 新旧程序先后尝试启动同一数据目录
- **THEN** 只有一个写入者，隔离目录实例不被错误阻止

### Requirement: 历史和事务
系统 SHALL 保留所有既有表与 history_imports，完整采样和 checkpoint 同事务，不重新导入已恢复历史。

#### Scenario: 旧库升级
- **WHEN** 旧 SQLite 副本交给 Rust 打开并持续采样
- **THEN** 旧日期总量/设置/账本不变，新增目标与应用逐桶对账一致

### Requirement: 备份与恢复
系统 SHALL 生成包含已提交 WAL 的一致性快照，重复恢复不双计，并保护升级后新增数据的回退边界。

#### Scenario: 一致性备份
- **WHEN** 数据库存在已提交WAL并执行备份
- **THEN** 快照完整且含最新提交，重复相同来源保持幂等
