# New API Status Bot

[![Build](https://github.com/lvyunqi/newapi-status-bot/actions/workflows/build.yml/badge.svg)](https://github.com/lvyunqi/newapi-status-bot/actions/workflows/build.yml)

基于 [QimenBot](https://github.com/lvyunqi/QimenBot) 开发的 New API 模型状态监控动态插件。

插件以 QimenBot ABI 0.3 动态插件形式编译为 `cdylib`，可通过 `/plugins reload` 热重载，
不需要修改 QimenBot 框架源码，也不需要加入 QimenBot 根 Cargo workspace。

## 功能

- 按本地模型白名单调用 New API 管理日志接口，避免抓取无关模型日志。
- 按模型和分组统计 15 分钟、1 小时、24 小时和 7 天窗口。
- 统计请求正常率、错误率、尝试错误率、重试率和部分失败数。
- 统计首字耗时（TTFT）和总体耗时的平均值、P50、P95。
- 根据 `request_id` 合并失败尝试和最终消费日志，区分最终失败与重试成功。
- 解析并聚合 `other.admin_info.use_channel` 重试渠道链。
- 使用 SQLite 保存最近 7 天历史，支持去重、游标恢复和版本迁移。
- 提供 QQ 群查询、采集健康检查、手动刷新和定时状态推送。
- 单模型查询可附带模型广场 `perf-metrics` 数据，并与本地日志指标分开显示。

## 运行要求

- QimenBot 动态插件 API 0.3。
- Rust 1.89 或更高版本，Edition 2024。
- New API 管理员 UID 和管理访问令牌。
- 推送功能需要 OneBot 实现端发送 `meta/Heartbeat` 事件。

## 构建

```powershell
cargo test --offline
cargo build --release
```

构建产物：

| 平台 | 文件 |
| --- | --- |
| Windows | `target/release/qimen_dynamic_plugin_newapi_status.dll` |
| Linux | `target/release/libqimen_dynamic_plugin_newapi_status.so` |
| macOS | `target/release/libqimen_dynamic_plugin_newapi_status.dylib` |

GitHub Actions 会在 Linux、Windows 和 macOS 上自动构建。普通分支构建可在工作流的
Artifacts 中下载；推送 tag 后会自动创建 GitHub Release 并上传三平台压缩包。

## 安装

1. 将对应平台的动态库复制到 QimenBot `plugins/bin/`。
2. 将 `config.example.toml` 复制为 QimenBot `config/plugins/newapi-status-bot.toml`。
3. 按实际服务填写模型白名单、分组、QQ 权限和推送参数。
4. 在 QimenBot 进程环境中设置管理访问令牌。
5. 启动 QimenBot，发送 `/plugins` 确认插件 ID `newapi-status-bot` 已加载。

升级插件时覆盖动态库，然后由 QimenBot 管理员发送：

```text
/plugins reload
```

插件关闭钩子会先唤醒并等待采集线程退出，宿主随后再卸载旧动态库。

## 配置

完整示例见 [`config.example.toml`](config.example.toml)。

管理访问令牌只从环境变量读取，不应写入 TOML：

```dotenv
NEWAPI_STATUS_ACCESS_TOKEN=replace-with-management-access-token
```

该令牌是 New API 用户设置中的系统管理访问令牌，不是模型调用使用的 `sk-` 令牌。
管理请求按照 New API 接口约定发送原值 `Authorization` 和 `New-Api-User` 请求头。

模型白名单通过多个 `[[models]]` 配置：

```toml
[[models]]
name = "gpt-5.6-terra"
display_name = "GPT-5.6 Terra"
groups = ["Codex Burst", "Codex Plus"]
max_ttft_ms = 12000
max_total_ms = 30000
```

`name` 必须与 New API 日志的 `model_name` 完全一致。`groups` 留空时展示日志中发现的
全部分组；填写后只统计指定分组，并始终保留无法确认归属的“自动路由/未确认”错误。

常用配置区域：

| 区域 | 用途 |
| --- | --- |
| `[api]` | New API 地址、管理员 UID、轮询、分页和回溯窗口 |
| `[storage]` | SQLite 文件和历史保留天数 |
| `[status]` | 样本数、成功率、TTFT、总耗时和过期阈值 |
| `[perf_metrics]` | 模型广场补充查询与缓存 |
| `[bot]` | 可查询群和管理员 QQ 白名单 |
| `[push]` | 推送模式、目标群、发送机器人和冷却时间 |

## 命令

| 命令 | 说明 |
| --- | --- |
| `/模型状态 [1m\|5m\|10m\|15m\|1h\|24h\|7d] [模型名]` | 查看白名单模型及分组状态 |
| `/模型列表` | 查看模型白名单、别名和配置分组 |
| `/模型异常 [模型名] [1m\|5m\|10m\|15m\|1h\|24h\|7d]` | 查看脱敏错误分类和高频重试链 |
| `/监控健康` | 管理员查看采集器、数据库和推送心跳 |
| `/监控刷新` | 管理员立即唤醒后台采集线程 |

普通查询同时支持群聊和私聊：群聊受 `bot.allowed_group_ids` 限制，私聊受
`bot.admin_user_ids` 限制；对应列表为空时不限制。管理命令还需要 QimenBot
管理员角色，并会额外校验 `bot.admin_user_ids`（列表为空时不额外限制）。

## 推送模式

| 模式 | 行为 |
| --- | --- |
| `periodic` | 按固定周期发送完整状态 |
| `change` | 模型状态确认发生变化后发送 |
| `anomaly` | 异常或数据过期确认后发送，并在恢复时通知 |

状态变化需要连续命中配置次数后才会发送，且受冷却时间和报告指纹去重保护。
多机器人环境应设置 `push.sender_self_id`，避免同一 Heartbeat 被多个机器人重复推送。

## 指标口径

New API 每次失败渠道尝试会产生一条错误日志，最终成功时还会产生消费日志。插件按
`request_id` 合并这些记录：

- 请求正常率按最终请求结果计算。
- 尝试错误率按原始渠道尝试计算。
- 重试率表示发生过多次渠道尝试的请求比例。
- `stream_status=error` 的消费日志记为部分失败。
- 只有错误日志且超过结算宽限期的请求记为最终失败。
- 无法确认分组的跨组重试错误归入“自动路由/未确认”。

`other.frt` 按毫秒统计 TTFT；`use_time` 来源为秒，因此总体耗时保持秒级精度显示。
状态分为正常、波动、异常、暂无样本、数据过期和样本不足。

模型广场 `/api/perf-metrics` 只在单模型查询时作为独立参考，不参与本地日志成功率、
延迟或状态判定。

## 数据与安全

- SQLite 默认位于 QimenBot 数据目录下的 `newapi-status-bot/status.db`。
- 默认保留 7 天数据，每日清理过期记录并执行 WAL checkpoint。
- 不保存访问令牌、IP、完整请求内容或原始管理接口响应。
- 错误摘要会脱敏并截断；重试链只保存正整数渠道 ID，最多 16 跳。
- 管理令牌、SQLite 文件、动态库和本地插件配置均不应提交到 Git。

## 故障排查

- 插件提示缺少配置：确认文件名为 `config/plugins/newapi-status-bot.toml`。
- 鉴权失败：确认使用管理访问令牌、管理员 UID 正确，且令牌位于 QimenBot 进程环境。
- 模型没有样本：确认 `[[models]].name` 与日志 `model_name` 完全一致。
- 数据持续过期：检查管理日志接口、网络代理和采集器错误分类。
- 推送等待 Heartbeat：确认 OneBot 实现端发送元事件，并检查 `push.sender_self_id`。
- 动态库无法加载：确认 QimenBot 与插件的操作系统、CPU 架构和 ABI 版本一致。

## 验证

```powershell
cargo fmt --all --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --release
```

真实管理日志测试默认忽略，只有显式设置测试开关、管理令牌和白名单模型后才会执行。
