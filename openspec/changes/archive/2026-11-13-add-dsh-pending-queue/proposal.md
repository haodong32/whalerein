# 方案提议

Decision status and confirmed delivery policy: [Decision Review](design.md#decision-review). Confirmation applies to the listed choices, not every draft detail; runtime acceptance still requires evidence.

## 背景与动机

WhaleRein 目前仅展示排队提示词数量，远程用户无法查看或撤回过时指令。DSH 的 cancel 会保留收件箱（原生控制器 `lib/index.js:960-964`），因此中断后需要能够查看待执行工作，并针对具体条目修改。

## 变更内容

- 增加已授权的实时待执行队列读取，返回不透明条目身份、位置和规范化内容。
- 增加写入命令日志的文本编辑、定向移除，以及将符合条件的下一轮用户指令提升为当前轮引导。
- 提供每个会话的读取、编辑、移除和提升能力；明确报告不支持或不可用状态，并返回有界完整响应。
- 变更后重新读取来源状态；区分条目缺失或已消费与队列不可用，并在变更结果不确定时保留 unknown。
- 不包含任意重排、批量清空、原子性的停止并清空、编辑上下文或系统条目、上传附件，以及第二套持久化队列。

## 能力范围

### 新增能力

- `pending-queue`：权威队列查看，以及针对待执行用户指令的安全定向变更。

### 修改的既有能力

无。本地主规格清单为空。

## 影响范围

Reuse shared invocation identity and optional correlation from [delivery](../add-dsh-message-delivery/design.md); implement the shared seam with the first consumer, without blocking queue reading on the send feature. 增加来源无关的队列查询和变更消息、SourceAdapter 队列读取能力、Host 命令分发、Core 类型化 API 及 CLI 队列命令。DSH 适配器映射原生 state 和 updateQueue；Access 仍为不解析应用内容的中继。不增加基础设施或明文存储。

已检查的可行性：本机安装的 DSH 0.2.2-rc.1 控制器 `lib/index.js:903-953` 支持文本 edit/remove/steer 并检查条目是否存在；`lib/types/types.d.ts:568-587` 区分 queue=null 与空队列，并保留条目位置。尚未验证原生集成。参见 [Harness 契约](../../../docs/architecture/harness-contract.md)、[DSH 架构](../../../docs/architecture/dsh-plugin.md)、[Core](../../../docs/architecture/core.md)和 [CLI](../../../docs/architecture/cli.md)。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.
