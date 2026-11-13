# 模型与推理强度切换方案

Decision status and confirmed delivery policy: [Decision Review](design.md#decision-review). Confirmation applies to the listed choices, not every draft detail; runtime acceptance still requires evidence.

## 背景与目标

远程用户目前必须回到 DSH 才能查看或切换会话使用的模型。`host/dsh/src/adapter/base.js`、`core/src/api/core.rs` 和 `apps/cli/src/cli.rs` 的现有控制面尚无模型操作，但本机安装的 DSH 0.2.2-rc.1 控制器已提供模型目录和选择接口。

## 变更内容

- 增加经过认证的类型化模型目录、会话模型选择读取，以及写入命令日志的模型切换命令和对应 CLI 操作。
- 展示可用推理档位，并返回执行源实际接受的归一化选择。
- Follow DSH's native model-selection behavior, including its attempt to save the default in the background. Display that effect and report default persistence as unconfirmed; no additional acknowledgement parameter is required.
- Let the native controller decide whether to accept a selection. WhaleRein does not add idle, empty-queue, or no-pending-input preconditions. Existing transport authorization, payload validation, capability checks, and command recovery still apply.
- 保留现有通道授权、不确定命令恢复和离线只读语义。
- 不包含提供方凭据管理、提供方配置、自动回退或重试、权限更改，也不绕过控制器实现“仅当前会话”的模型切换。

## 能力范围

### 新增能力

- `session-model-selection`：与执行源无关的模型发现、读取和显式模型/推理档位选择，准确报告副作用与恢复状态。

### 修改的现有能力

无。本地主规格清单为空；本变更依据已检查的实现新增增量规格，不把其他方案视为已经实现的行为。

## 影响与依赖

涉及 `proto/v1/whalerein.proto` 及生成绑定、`host/runtime/src/contracts/source.js` 和命令/查询分发、`host/dsh/src/adapter/real.js`、`base.js`、`fake.js`、Core 公共 API、CLI 参数与输出，以及契约和集成测试夹具。Access 保持不解释业务内容的中继职责，不新增模型状态或服务。

依赖 `add-dsh-session-creation` 中提出的 `host_capabilities` 查询及 Host 能力约定，不依赖创建会话这一操作本身。若 `add-dsh-message-delivery` 先落地，则复用其调用上下文约定；即使原生接口没有请求 ID 去重，模型选择也必须正确工作。整个变更组在实现阶段统一分配 protobuf 字段编号。

架构参考：[总览](../../../docs/architecture.md)、[Harness 契约](../../../docs/architecture/harness-contract.md)、[DSH 插件](../../../docs/architecture/dsh-plugin.md)、[Core](../../../docs/architecture/core.md)、[CLI](../../../docs/architecture/cli.md) 和[测试要求](../../../docs/testing.md)。这些是方案材料，不构成原生集成或部署行为已通过验证的声明。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.
