# 变更提案

Decision status and confirmed delivery policy: [Decision Review](design.md#decision-review). Confirmation applies to the listed choices, not every draft detail; runtime acceptance still requires evidence.

## 背景与动机

受远程控制的会话可能需要结构化人工输入，而 WhaleRein 当前无法展示或回答这些问题：`host/dsh/src/adapter/real.js` 仅桥接工具审批请求。已安装 DSH 提供用户问题投影、open 请求瀑布流，以及独立的 continued 问题回答服务。

## 变更内容

- 暴露作用域内的待处理问题批次，提供稳定的不透明身份、原生选项/自由文本上下文、状态，以及版本/摘要守卫。
- 增加显式远程回答提交，使用原生校验、去重命令回执及本地/远程结算协调。
- 仅在原生仲裁和生命周期路径验证通过后支持 open 与 continued 问题；旧提交失效后，不将一种问题重新解释为另一种。
- 增加 Core 列出/回答 API 和 CLI `questions` / `answer`，从文件或标准输入读取结构化输入；通过独立的会话等待变更提供需要输入的原因。
- 不包括自动回答、审批策略修改、任意原生载荷透传、提供方工具重放或新的持久化问题存储。

## 能力范围

### 新增能力

- `user-questions`：通过归一化、有守卫的远程控制发现并显式回答原生结构化问题批次。

### 修改的既有能力

无；本地主规格清单为空。

## 影响范围

增加协议问题/回答类型和每会话能力、适配器问题桥接和原生投影读取、Host 命令路由/去重、Core 状态/API 校验及 CLI 输出。复用加密传输和命令恢复。问题与回答始终属于内容，不进入诊断或运行明文状态。参见 [Harness 契约](../../../docs/architecture/harness-contract.md)、[DSH 插件架构](../../../docs/architecture/dsh-plugin.md)、[Core 架构](../../../docs/architecture/core.md)和[安全规范](../../../docs/security.md)。

Shares input-reason types with waiting without requiring its operation to ship first; 若已有 `add-dsh-message-delivery` 的调用身份上下文，则复用它，不暴露原生请求 ID。原生包接口只能建立可行性，不能证明真实本地/远程竞争的正确性。准确的 open 请求仲裁是阻塞发布的可行性门槛，启用能力前必须验证；不得抢占本地回答器，也不得公告合成的问题实现。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.
