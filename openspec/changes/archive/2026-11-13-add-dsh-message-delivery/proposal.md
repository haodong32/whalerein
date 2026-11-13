# 方案提议

Decision status and confirmed delivery policy: [Decision Review](design.md#decision-review). Confirmation applies to the listed choices, not every draft detail; runtime acceptance still requires evidence.

## 背景与动机

远程用户目前无法在 DSH 会话运行中追加指令：`host/dsh/src/adapter/base.js:59-64` 仅接受 `START_IF_IDLE`，并拒绝忙碌会话。开放协议中已定义的排队和当前轮引导模式，可以让用户继续协作，而不必打断已有工作。

## 变更内容

- 通过 Rust Core 和 CLI 显式提供仅空闲发送、下一轮排队、当前轮引导三种投递方式；default to native queue; start-if-idle is explicit only. This intentionally changes the former implicit default, while explicit START_IF_IDLE requests retain their meaning.
- 公布实时适配器支持的模式，并完全在适配器内部映射到 DSH 原生 `prompt` 调用。
- 返回具有稳定关联身份的接收回执，区分接收与生成完成，并保留命令结果的不确定性。
- 区分原生收件箱条目 ID、公共命令 ID 和已提交 Message ID，建立待执行队列管理可复用的关联约定。
- 不包含队列编辑或移除、附件、自动重试、创建会话，也不保证引导指令立即改变模型行为。

## 能力范围

### 新增能力

- `message-delivery`：显式指令投递模式、能力发现、接收反馈与恢复。

### 修改的既有能力

无。本地主规格清单为空；本能力以当前代码为依据，记录预期行为。

## 影响范围

涉及 `core/src/api/core.rs`、公共类型化命令 API、`apps/cli/src/{cli,run,output}.rs`、共享 Host 的来源和命令边界、DSH 真实与模拟适配器、原生上下文测试以及跨语言协议测试夹具。复用 `proto/v1/whalerein.proto:568` 中既有 `DeliveryMode` 值；新增回执或上下文保持来源无关并采用增量扩展。Access 仍为不解析应用内容的中继，不新增基础设施。

参考：[Harness 契约](../../../docs/architecture/harness-contract.md)、[DSH 架构](../../../docs/architecture/dsh-plugin.md)、[Core](../../../docs/architecture/core.md)、[CLI](../../../docs/architecture/cli.md)、[术语表](../../../docs/glossary.md)。

已观察到的可行性：本机安装的 DSH 0.2.2-rc.1 控制器 `lib/index.js:831-872` 接受 queue/steer，并确认指令接收。这属于静态证据，不是原生集成验证。发布时启用新模式前，需要通过有界原生验证确认引导时的阶段变化，以及请求与收件箱条目的关联。
