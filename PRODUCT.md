# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

用户已选择 Tauri 2 + React + TypeScript + Tailwind CSS + 完整 Rust 后端。首期 Windows 完整交付，保留三端平台适配接口。

## Users

当前 Windows 用户，希望理解 Mihomo 已确认代理流量由哪些应用及访问目标产生，查看历史和采集状态。

## Product Purpose

提供本地优先、持续托盘采集、可按日期／小时／五分钟下钻的流量面板。统计口径和历史保留是迁移契约。

## Capabilities and Constraints

界面中文。保留24小时、7/30日、本周、本月趋势，应用与目标排行、整列悬停、键盘操作、提醒与登录自启。真实流量和控制密钥不进入演示、日志、截图夹具。窗口关闭隐藏，托盘退出才停止采集。核心和唯一 SQLite 写入者由 Rust 管理。

## Brand Commitments

名称保持「Clash 流量监控」。用户选择现代、简洁、接近 macOS 质感的界面，实际系统能力以 Windows 为准。

## Evidence on Hand

原始 ui.py、storage.py 和已有测试定义实际查询、下钻与数据质量口径。没有用户提供的新图标或视觉稿。视觉配色及布局属于本次实施选择，不冒充用户逐项确认。

## Product Principles

- 代理、直连、未知出口的含义清楚。
- 采样质量始终可见，零流量与未采集可以区分。
- 日到小时再到应用／目标的上下文连续。
- 图表动效不影响实时阅读或后台采集。
