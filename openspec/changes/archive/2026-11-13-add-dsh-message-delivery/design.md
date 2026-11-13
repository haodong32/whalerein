# 设计

## Decision Review

Confirmed rows record only the choices listed below. Other draft choices still need evidence or review.

| ID | Decision / recommendation | Alternatives / tradeoffs | Status | Basis | Affected acceptance |
|---|---|---|---|---|---|
| D-REL-01 | Deliver verified capabilities independently; keep unverified capabilities disabled and report them. Dependencies still apply. | Deliver all seven together, or wait for all native probes. | CONFIRMED | User reply in this chat on 2026-11-11. This confirms delivery policy, not runtime success or blanket approval of the plan. | All ACs |
| D-DELIVERY-01 | Default send uses native queue; steer delegates native behavior even while idle. START_IF_IDLE remains an explicit observed-state convenience, not atomic admission. | Retaining the old implicit start-only default would diverge from native sending. | CONFIRMED | Explicit user choice during this review; native prompt()/agent followup()/steer() inspected. | AC-01–AC-04, AC-10, AC-11 |


## 上下文

参见[方案提议](proposal.md)。当前 Core 的 `send_message` 固定使用 StartIfIdle（`core/src/api/core.rs:759-770`）；CLI send 没有模式参数（`apps/cli/src/cli.rs:76-83`）。Host 的 `CommandService` 在调用 `SourceAdapter.execute` 前写入命令日志，且已区分确定性失败与不确定结果。DSH 适配器每次调用都会创建新的原生 requestId（`host/dsh/src/adapter/real.js` 中的 sendUserMessage）。原生控制器 0.2.2-rc.1 将 queue 分发到 followup，将 steer 分发到 agent.steer（`lib/index.js:831-872`）。这些是已检查的源码事实，不是运行时验证结果。

## 目标与非目标

**目标：** 通过现有安全控制路径提供类型化且可发现的投递意图；公开接收关联身份，同时避免混淆公共命令和 Message。

**非目标：** 不增加调度器、提供方抽象、明文指令日志、附件或自动重试。不新增 Access 路由。Host 预检不宣称具有来源阶段原子锁。

## 架构审查

| 边界 | 职责与依赖方向 |
|---|---|
| CLI → Core | CLI 解析 `send --delivery start-if-idle\|enqueue-after-run\|steer`，输出接收或状态元数据，不回显文本。Default CLI/Core send uses queue; start-if-idle is explicitly requested only.Core 提供类型化、支持模式的方法；既有 send_message 委托默认模式。 |
| Core → 共享 Host | 复用 SendMessage.delivery_mode。Core 根据能力改善交互，但 Host 和来源负责权威验证；过期缓存中的能力不授予操作权限。 |
| Host → SourceAdapter | 保留命令接收日志与错误语义。为 execute 增加可选的规范化调用上下文，携带由 Host、设备、命令身份派生的稳定不透明操作身份。调用上下文不包含指令正文。现有会话级 execute 仍是边界。 |
| DSH 适配器 → 原生控制器 | 仅在原生 prompt 服务可用时公布模式。在内部映射 queue 和 steer，将稳定调用身份映射到原生 requestId；不向 Core 传递 DSH 原生 JSON 或位置。 |
| 状态归属 | DSH 拥有待执行指令和执行状态。Host 仅拥有命令状态与身份元数据；Core 保留既有加密缓存及恢复状态。Access 仍不解析应用内容。 |

遵循 [Harness 契约](../../../docs/architecture/harness-contract.md)、[Core 指南](../../../docs/architecture/core.md)和 [CLI 指南](../../../docs/architecture/cli.md)。

## 决策

1. **Native default (D-DELIVERY-01).** New CLI/Core calls without a delivery choice use ENQUEUE_AFTER_RUN, mapping to native queue. Treat an omitted/UNSPECIFIED mode consistently at the protocol boundary; keep the existing explicit enum values unchanged. Reject unknown numeric values and unsupported requested modes rather than falling back. Existing clients explicitly sending START_IF_IDLE retain that opt-in behavior.
2. **Preserve native admission.** Map queue to followup and steer to steer without a running/idle precheck. Both can wake an idle Agent; native send can redirect a wake after abort to next-turn. Do not reject or relabel this native behavior. Only explicit START_IF_IDLE uses a sampled-state precheck followed by native queue; it cannot promise atomic exclusion against local starts. DSH owns the inbox and scheduling.
3. 在规范化回执和 CommandHandle 中增加 `delivery_id`，作为不透明接收关联身份。为兼容性保留既有 CommandAccepted.message_id，但不再将其解释为已提交 Message 身份；当前适配器使用原生 requestId 填充该字段。不合成已提交 Message 或历史游标。原生收件箱条目 ID 是独立身份，后续由待执行队列能力通过原生 rpcId 建立关联。
4. 为每个已写入日志的命令，从规范化调用上下文派生一个稳定操作身份，用作原生 requestId。若需要持久化，仅保留身份元数据；继续采用现有基于摘要的去重。不要在不同设备或 Host 间单独复用公共命令 ID。其他变更操作可复用该约定，而无需解析原生状态。
5. 除非已证明没有接收指令，否则不把抛出的原生错误标记为确定性失败。原生回执丢失、响应编码失败和终态存储失败均保留 unknown。即使原生 API 提供请求去重，也不允许重启后自动重新提交。

## 验收审查

| AC编号 | 场景与预期结果 | 验证方式 | 来源场景 |
|---|---|---|---|
| AC-01 | Default sending: an omitted CLI/Core mode uses native queue for idle and running sessions; receipt is acceptance, not execution completion. | 单元测试，加协议和恢复集成测试 | [场景](specs/message-delivery/spec.md#ac-01) |
| AC-02 | 下一轮投递：接收回执标识该次投递，不中断当前生成；执行遵循来源队列，不报告为已经完成。 | 原生发布验证流程，加模拟和契约测试 | [场景](specs/message-delivery/spec.md#ac-02) |
| AC-03 | 当前轮引导：来源的引导路径接收一次指令，结果报告接收成功，但不承诺模型立即遵从。 | 原生发布验证流程，加模拟和契约测试 | [场景](specs/message-delivery/spec.md#ac-03) |
| AC-04 | Idle steering: invoke native steer, which can wake execution; report acceptance rather than conflict and do not invent a different wrapper mode. | 原生发布验证流程，加模拟和契约测试 | [场景](specs/message-delivery/spec.md#ac-04) |
| AC-05 | 不支持或无效的模式：命令分别返回 unsupported_capability 或 invalid_argument，既有仅空闲发送客户端继续正常工作。 | 单元测试，加协议和恢复集成测试 | [场景](specs/message-delivery/spec.md#ac-05) |
| AC-06 | 稳定的投递回执：返回相同投递身份；接收状态和该身份均不宣称已有已提交回复。 | 单元测试，加协议和恢复集成测试 | [场景](specs/message-delivery/spec.md#ac-06) |
| AC-07 | 重复请求与变更载荷的重放：首个请求最多接收一次；相同重复请求返回已记录结果，变更载荷则以 payload_digest_mismatch 拒绝。 | 单元测试，加协议和恢复集成测试 | [场景](specs/message-delivery/spec.md#ac-07) |
| AC-08 | 回执丢失与重启：可恢复结果保持 unknown，或返回随后确认的结果；任何恢复路径均不第二次调用原生接口。 | 单元测试，加协议和恢复集成测试 | [场景](specs/message-delivery/spec.md#ac-08) |
| AC-09 | 授权与隐私：未授权投递不会到达 DSH；诊断输出不包含指令正文或传输凭据。 | 单元测试，加协议和恢复集成测试 | [场景](specs/message-delivery/spec.md#ac-09) |
| AC-10 | 输入限制与竞争反馈：无效输入返回 invalid_argument 或 payload_too_large；阶段竞争返回真实来源回执、已证明的确定性来源拒绝，或在无法确认是否接收时返回 unknown，绝不虚构生成完成。 | 原生发布验证流程，加模拟和契约测试 | [场景](specs/message-delivery/spec.md#ac-10) |
| AC-11 | Explicit start-if-idle: reject a precheck-observed running state before native submission; otherwise call native queue and report its actual result without claiming the precheck excluded a concurrent local start | CLI/Core/adapter tests and controlled native race checks | [Scenario](specs/message-delivery/spec.md#ac-11) |

## 风险与权衡

- [原生引导的调度行为随阶段变化] → 发布前保持该模式禁用，直到真实 DSH 流程验证运行中、空闲、完成竞争和取消行为。绝不承诺模型立即遵从。
- [原生接收与队列关联] → 验证 requestId 在消费后仍可保留，并作为 rpcId 出现在排队条目上；队列条目 ID 继续独立。无法建立关联时，阻止公开该映射，但不必阻止所有投递回执。
- [命令恢复仍有进行中的修改] → 实现前与当前工作区协调，扩展既有持久化意图路径，不以第二套日志替代。
- [尚未发布的增量线协议变更] → 复用 v1，仅在回执变更时重新生成绑定。Explicit legacy START_IF_IDLE requests retain their opt-in behavior; a source missing queue support rejects new default queue requests；未公布模式明确失败。

## 迁移计划

先实现来源无关的枚举传递、可选回执和上下文扩展，再实现适配器与 CLI。在客户端公布控制项前部署 Host 支持；Clients explicitly sending START_IF_IDLE retain that opt-in value. New CLI/Core defaults intentionally use queue; reject a missing queue capability rather than silently reverting. 运行协议一致性与完整跨组件集成验证。回滚时先撤回客户端和能力公布，再回滚 Host 代码；保留命令日志和 unknown 结果。不需要迁移会话历史或新增基础设施。

## Agent 执行说明

有界原生可行性验证属于发布门槛：创建一次性会话，通过每种模式提交不同的合成指令，检查收件箱位置和身份，触发完成与预检竞争，重新连接，并确认结果恢复不会重复接收。仅在 `.local-docs/acceptance/` 保存元数据证据，不保存私有提示词或工具参数。若原生行为与约定结果不符，在启用该模式前停止并提出方案修订。

验证命令依据仓库执行器契约选择：`./repo proto --check`、`./repo check --project core --project cli --project proto`、`./repo test --project core --project cli --project proto --project host --project dsh --coverage`、`./repo test --suite integration --group protocol`，然后运行 `./repo test --suite integration`。Node host/dsh 未注册静态检查或构建脚本，因此不选择不支持的 Node 检查。每个受影响的第一方包或 crate 覆盖率必须达到 90%。本次规划期间未运行这些检查。

## Native parity review evidence

Controller prompt() maps queue/steer to Agent followup/steer; dsh-agent-loop/lib/index.js:564-634 places next-turn/next-step and wakes idle execution, including next-turn redirection after abort. D-DELIVERY-01 changes the implicit default; explicit START_IF_IDLE is a documented WhaleRein sampled-state extension, not a native atomic mode.

Evidence is from the locally installed DSH 0.2.2-rc.1 packages, reviewed read-only. Native runtime/integration acceptance has not run. Existing transport authorization, command deduplication, encryption, privacy and uncertain-outcome recovery remain intentional remote-adapter boundaries.

## Capability acceptance gates

These are logical operation gates, not mandatory new wire flags. Follow the [evidence contract](../../README.md#acceptance-evidence-contract): all listed AC variants need current evidence, and source behavior needs the mapped native procedures. A flag covering several operations is enabled only when all its operation gates pass. Negative unsupported-capability tests passing do not enable an unverified operation. Existing shipped operations remain governed by their existing support; report any regression separately.

| Operation | Required ACs (all mandatory variants) | Required native evidence subset |
|---|---|---|
| Queue/default send | AC-01, AC-02, AC-05–AC-10 | AC-01, AC-02, AC-06, AC-10 |
| Direct steer | AC-03–AC-10 | AC-03, AC-04, AC-06, AC-10 |
| Explicit START_IF_IDLE | AC-05–AC-11 | AC-06, AC-10, AC-11 |

Record each operation as enabled or disabled, its gate evidence, blockers and tested native version in the delivery summary. Keep any disabled path unavailable and retain FAIL/BLOCKED/NOT RUN results. A partial rollout is not full acceptance. The [combined workflow cases](../add-dsh-session-creation/design.md#combined-workflow-acceptance) qualify the integrated feature set separately and do not block independent delivery of verified operations.
