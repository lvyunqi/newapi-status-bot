# New API Status Bot

QimenBot 动态插件。插件定期读取 New API 管理日志，按本地模型白名单和分组统计
请求正常率、错误率、重试率、首字耗时和总体耗时，并在 QQ 群提供状态查询与推送。

公开 `perf-metrics` 接口只在查询单个模型时作为独立参考，不参与日志指标计算。

## 构建

```powershell
cargo test
cargo build --release
```

输出文件：

- Windows: `target/release/qimen_dynamic_plugin_newapi_status.dll`
- Linux: `target/release/libqimen_dynamic_plugin_newapi_status.so`
- macOS: `target/release/libqimen_dynamic_plugin_newapi_status.dylib`

将文件复制到 QimenBot 的 `plugins/bin/`。本项目是独立工作区，不需要修改 QimenBot
源码或根工作区成员列表。

## 配置

将 `config.example.toml` 的内容放入 QimenBot：

```text
config/plugins/newapi-status-bot.toml
```

管理访问令牌只通过环境变量提供：

```dotenv
NEWAPI_STATUS_ACCESS_TOKEN=replace-with-a-new-management-access-token
```

该令牌必须是 New API 个人设置中生成的管理访问令牌，而不是模型调用 `sk-` 令牌。

数据库默认写入宿主数据目录下的 `newapi-status-bot/status.db`，本地历史保留 7 天。
示例配置只包含一个演示白名单模型；上线前应按实际可见模型补齐 `[[models]]`，并为
每个模型填写需要展示的分组。未配置分组时会展示日志中发现的全部分组。

## 安装与热重载

1. 将 Release 动态库复制到 QimenBot 的 `plugins/bin/`。
2. 将 `config.example.toml` 复制为 `config/plugins/newapi-status-bot.toml` 并调整白名单。
3. 在启动 QimenBot 的进程环境中设置 `NEWAPI_STATUS_ACCESS_TOKEN`。
4. 启动后发送 `/plugins`，确认 `newapi-status-bot` 已加载。

后续升级时先覆盖动态库，再由 QimenBot 管理员发送 `/plugins reload`。插件的
`#[shutdown]` 会唤醒并等待采集线程退出，然后宿主才卸载旧 DLL。

## 命令

```text
/模型状态 [15m|1h|24h|7d] [模型名]
/模型列表
/模型异常 [模型名] [15m|1h|24h|7d]
/监控健康
/监控刷新
```

普通命令受 `bot.allowed_group_ids` 限制。管理命令使用 QimenBot 的管理员角色，并可
额外受 `bot.admin_user_ids` 限制。

## 故障排查

- 提示缺少配置：确认文件名严格为 `config/plugins/newapi-status-bot.toml`。
- `/监控健康` 显示缺少环境变量：确认令牌设置在 QimenBot 进程环境，而不是只在当前终端。
- 数据持续过期：检查管理令牌权限、`api.base_url`、管理员 UID 和 New API 日志接口状态。
- 模型始终无样本：确认日志中的 `model_name` 与本地 `[[models]].name` 完全一致。
- 推送显示等待 Heartbeat：确认 OneBot 实现会发送元事件，并检查 `push.sender_self_id`。
- DLL 无法加载：确认插件与 QimenBot 使用相同平台和架构，并重新执行 Release 构建。

## 数据口径

New API 会为每次失败渠道尝试写一条错误日志，并在最终成功时另写消费日志。本插件
使用 `request_id` 归并最终结果：用户看到的正常率是请求级指标，渠道失败日志另用于
计算尝试错误率和重试率。

完整实现顺序和验收条件见 [TODO.md](TODO.md)，架构边界见
[docs/DESIGN.md](docs/DESIGN.md)。

## 当前状态

本地离线测试、严格 Clippy、Release 构建和 Windows DLL 导出检查均已通过。真实管理
日志与 QQ 收发联调仍需要新的有效管理访问令牌和上线群配置，未完成项以 `TODO.md`
为准。
