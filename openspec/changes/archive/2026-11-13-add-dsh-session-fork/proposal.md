# 变更提案

Decision status and confirmed delivery policy: [Decision Review](design.md#decision-review). Confirmation applies to the listed choices, not every draft detail; runtime acceptance still requires evidence.

## 背景与动机

远程用户目前无法利用既有对话尝试另一种方案，除非手动重建上下文。DSH 已有原生分叉操作，但 WhaleRein 没有暴露分叉控制；增加这一能力可保持 DSH 的权威地位，并避免通过第二套会话存储复制对话内容。

## 变更内容

- 增加带类型、会话范围内的分叉控制，使用原生最近完整轮次边界，返回新会话身份和父子关系。
- 复用 `add-dsh-session-creation` 的能力查询和生命周期回执约定，以及既有命令去重和结果恢复。
- 通过既有 list/get/history 发现和读取子会话；源历史及其缓存身份保持不变。
- 后续工作区绑定失败时报告子会话的部分创建结果；无法获得可靠子身份时保留不确定性。
- 不包含任意事件／消息切点选择、原生序号字段、隐式首条消息、模型／预设覆盖、文件隔离、worktree 创建、重命名、回滚或删除。

## 能力范围

### 新增能力

- `session-fork`：从最近完整轮次创建由 DSH 拥有的新对话，具有明确父身份及安全的命令／结果恢复。

### 修改的能力

无。本地主规范清单为空。

## 影响范围

依赖 `add-dsh-session-creation` 提供共享主机能力发现和带类型的生命周期回执约定；分叉执行仍处于会话范围。扩展 protobuf、`SourceAdapter.execute(sessionId, command)`、DSH 真实／模拟适配器、Rust Core、CLI 以及协议／全链路测试。Access 仍是经过授权的不透明中继，无需新依赖或基础设施。

本机安装的 DSH 0.2.2-rc.1 控制器 `lib/index.js:756-811` 会观察源会话、选择最近完整前缀、创建子会话；子会话创建后工作区绑定仍可能失败。这些已检查的代码路径仅证明可行性。真实 DSH 行为、父关系投影、待执行收件箱处理和冷会话持久化仍需要设计文档中的明确验收检查。

参考：[Harness 契约](../../../docs/architecture/harness-contract.md)、[DSH 架构](../../../docs/architecture/dsh-plugin.md)、[Core](../../../docs/architecture/core.md)、[CLI](../../../docs/architecture/cli.md)。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.
