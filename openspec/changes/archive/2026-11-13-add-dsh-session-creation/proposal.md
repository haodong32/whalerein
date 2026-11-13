# 变更提案

Decision status and confirmed delivery policy: [Decision Review](design.md#decision-review). Confirmation applies to the listed choices, not every draft detail; runtime acceptance still requires evidence.

## 背景与动机

WhaleRein 能列出并控制既有会话，但不能发起新的远程任务：`SourceAdapter` 没有生命周期创建操作，`CommandService.commandOperation` 只识别发送、中断和审批决定。远程创建会话可免去离开主机前预先准备会话的步骤。

## 变更内容

- 增加经过身份认证的主机能力查询，以及带类型、可去重的创建会话控制。Accept optional mutually exclusive cwd/workspace, optional preset and optional preferred session ID; omit location to use the Host default and delegate blank reuse to native create.
- Return the actual native session identity with READY/READY_WITH_WARNING; creation may reuse an eligible blank Session, so do not always claim a new identity. Send any first prompt separately and preserve partial workspace-attachment results.
- 增加 Rust Core 和 CLI 入口，通过既有 list/get/history 流程发现已创建会话。
- 保留加密传输、授权、持久化命令身份和未知结果恢复；不引入服务端会话存储。
- 不包含重命名、工作区／预设管理或发现、文件系统浏览、隐式执行首条消息、删除，以及使用新命令 ID 的自动重试。工作区和预设标识来自既有主机知识，目录路径由 DSH 解释。

## 能力范围

### 新增能力

- `session-creation`：Use native optional location, preset and preferred-ID create/reuse semantics for DSH-owned Sessions，涵盖能力发现、部分结果报告和安全恢复。

### 修改的能力

无。本地主规范清单为空；既有行为以当前实现为依据。

## 影响范围

涉及接口：`proto/v1/whalerein.proto` 及生成绑定；`host/runtime/src/contracts/source.js` 和 `commands/service.js`；DSH 真实／模拟适配器；`core/src/api/core.rs` 和命令恢复；CLI 解析／输出；既有协议和全链路集成夹具。这是 v1 领域协议的增量扩展，不新增 HTTP 路由或传输。Access 仍是不透明中继；验证既有请求类型／摘要授权及兼容性检查，不增加明文领域解释。

原生可行性依据是本机安装的 DSH 0.2.2-rc.1 控制器 `lib/index.js:666-697`：create 支持 cwd/workspace 和预设，创建后仍可能工作区绑定失败。这是静态证据，不是真实来源验收。设计文档记录了执行变更工作前的具体原生可行性检查。

参考：[Harness 契约](../../../docs/architecture/harness-contract.md)、[DSH 架构](../../../docs/architecture/dsh-plugin.md)、[Core](../../../docs/architecture/core.md)、[CLI](../../../docs/architecture/cli.md)、[Access](../../../docs/architecture/access.md)。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.
