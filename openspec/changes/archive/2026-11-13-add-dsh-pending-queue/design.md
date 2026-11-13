# 设计

## Decision Review

Confirmed rows record only the choices listed below. Other draft choices still need evidence or review.

| ID | Decision / recommendation | Alternatives / tradeoffs | Status | Basis | Affected acceptance |
|---|---|---|---|---|---|
| D-REL-01 | Deliver verified capabilities independently; keep unverified capabilities disabled and report them. Dependencies still apply. | Deliver all seven together, or wait for all native probes. | CONFIRMED | User reply in this chat on 2026-11-11. This confirms delivery policy, not runtime success or blanket approval of the plan. | All ACs |
| D-QUEUE-01 | Interrupt retains pending work; withdrawal is a separate explicit operation. | Stop-and-clear would be an additional composed operation with partial-failure semantics. | CONFIRMED | User confirmed after review of DSH 0.2.2-rc.1 cancel(), which calls agent.cancel with keepInbox: true. Runtime validation remains outstanding. | AC-08 |
| D-LIMITS-01 | Reuse verified DSH and existing protocol limits first. Investigate and separately justify any necessary additional bound; seek review when it changes observable behavior. | Earlier draft numbers were not established native constraints. | CONFIRMED | Explicit user choice in this chat; exact source limits still need evidence. | AC-11 |

## 上下文

参见[方案提议](proposal.md)。SessionSummary 当前仅包含 queued_prompts；DSH 适配器从 inbox 投影计算该数量。`SourceAdapter` 没有队列读取方法，`CommandService` 仅分发 send/interrupt/approval 控制。本机安装的 DSH 0.2.2-rc.1 `dsh-api-session-controller/lib/types/types.d.ts:568-587` 提供队列条目 id/placement/rpcId/content，并以 queue=null 表示被动状态不可用。`lib/index.js:903-953` 实现 edit/remove/steer，限制为纯文本编辑并检查仍待执行的条目；`:960-964` 使用 keepInbox=true 取消。这些是静态源码事实。

## 目标与非目标

**目标：** 通过现有加密请求和控制路径，提供在线、有界的队列视图及范围明确的定向变更。复用[投递身份约定](../add-dsh-message-delivery/design.md)。

**非目标：** 不增加第二套任务引擎、Host/Core 队列持久化、自动排空、重排、批量清空、附件上传、系统或上下文变更、CAS 版本或原子性的停止并清空。不需要全会话队列聚合器。

## 架构审查

| 边界 | 职责与状态归属 |
|---|---|
| CLI → Core | 增加 `queue list`、`queue edit`、`queue remove`、`queue steer`，显式指定会话与条目身份。列表展示规范化内容，因为这是用户请求的输出；变更回执和日志绝不回显内容。Core 返回类型化在线结果，不将队列条目视为已提交 Message。 |
| Core → 共享 Host | 增加来源无关的待执行队列读取、定向变更载荷和会话能力标志。不传递原生事件 JSON，不计算原生游标。继续强制执行既有授权和通道检查。 |
| Host → SourceAdapter | 为 SourceAdapter 增加有界 readPendingQueue(sessionId, limits)；会话级变更使用 execute，并携带投递方案中的规范化调用上下文。Host 仅记录一次变更调用，读取仍为查询。 |
| DSH 适配器 → 原生控制器 | 读取被动状态，依据既有安全投影规则规范化队列内容，将条目操作映射至 updateQueue；仅在已确认原生用户条目归属及内容支持时允许变更。来源不明的条目只读。 |
| 持久化 | DSH 拥有队列、顺序和原生变更语义。Host 仅保存既有身份、状态、密文运行数据。队列内容仅存在于临时加密响应和调用方内存；Core 不持久化队列缓存。Access 中继密文。 |

遵循 [Harness 契约](../../../docs/architecture/harness-contract.md)、[DSH 架构](../../../docs/architecture/dsh-plugin.md)和 [Core 架构](../../../docs/architecture/core.md)。

## 决策

1. 使用独立队列查询，不在 RuntimeState 心跳中加入完整待执行内容。队列快照包含会话身份、观察到的可用性和运行状态、有序条目、逐项操作及可选 delivery_id 关联。这既避免在普通 watch 刷新中传输待执行文本，也不与等待状态方案中的轻量状态分类竞争。
2. 将 queue_item_id 视为不透明且绑定会话的身份。原生条目 ID 和 rpcId 仅由适配器解释；可选 delivery_id 与投递方案关联，command_id 则继续作为独立日志身份。不使用列表下标执行变更。
3. 被动读取，区分 null 或不可用状态与可用空数组。队列查询不允许恢复或激活会话。条目变更可使用既有原生 updateQueue 路径，但必须披露并验证其恢复会话的行为。冷会话队列读取不能断言没有待执行工作。
4. **Use verified native/protocol bounds (D-LIMITS-01).** Remove the proposed 256-item and 1 MiB product limits. Inspect actual source and transport constraints, including encoded envelope overhead, before defining boundary fixtures. Reject an oversized complete response explicitly rather than silently truncating. Document any necessary additional bound with evidence for separate review; protocol response bounds do not prove bounded native allocation.
5. 仅对已可靠识别的待执行用户工作开放操作：Text edit follows native whole-content replacement: the replacement must be nonblank text, and editing an eligible user item replaces its complete original content, including attachments; display this effect explicitly；上下文或来源不明条目只读。只有适配器能确认安全归属及原生资格时，才允许移除或提升带附件的用户条目。不传递原始 JSON、隐藏工具参数或特权上下文内部内容；不支持的内容使用安全元数据。
6. 并发变更以原生来源顺序为准；已检查的 updateQueue 缺少预期版本或 CAS，因此不增加此类契约。预检可以改善错误反馈，但不能创造来源原子性。已证明缺失的条目映射为 not_found，阶段或位置不合格映射为 conflict，能力不支持映射为 unsupported_capability，文本无效映射为 invalid_argument。若提升前的移除可能已经发生，之后的失败属于 unknown，不是确定性拒绝。
7. 变更回执与后续读取独立返回。已确认变更不会因刷新失败而降级。CLI 随后读取新队列，或报告刷新不可用；不将过期数据展示为当前状态。重复命令恢复日志中的原结果；新的队列读取不能证明 unknown 编辑已经成功。
8. Interrupt 保持独立，且保留待执行工作。中断后列出剩余工作，允许显式定向撤回。替代方案是批量停止并清空；由于原生取消和逐项变更均没有经过检查、覆盖两者的事务，予以拒绝。

## 验收审查

| AC编号 | 场景与预期结果 | 验证方式 | 来源场景 |
|---|---|---|---|
| AC-01 | 完整且有界的队列读取：返回带有会话身份、已观察状态和条目操作的完整有序队列；不包含原生 JSON、工具参数或原生序列位置。 | 单元测试，加协议和全链路集成测试 | [场景](specs/pending-queue/spec.md#ac-01) |
| AC-02 | Native empty [] is available with zero items; cold null is unavailable for queue reading, not an empty queue. This does not prevent native wait from reporting cold idle. | 原生一次性会话流程，加确定性适配器测试 | [场景](specs/pending-queue/spec.md#ac-02) |
| AC-03 | Whole-content text replacement retains identity/source/placement; original attachments are replaced rather than retained. This is an explicit edit, not a second instruction. | 单元测试，加协议和全链路集成测试 | [场景](specs/pending-queue/spec.md#ac-03) |
| AC-04 | 定向移除：仅撤回该次待执行条目；其已提交对话历史保持不变。 | 单元测试，加协议和全链路集成测试 | [场景](specs/pending-queue/spec.md#ac-04) |
| AC-05 | 提升为当前轮引导：同一工作仅一次移动至引导，不产生第二个待执行副本；若能证明位置不合格或阶段不可用，则在变更前返回 conflict。 | 原生一次性会话流程，加确定性适配器测试 | [场景](specs/pending-queue/spec.md#ac-05) |
| AC-06 | 条目消费与并发编辑：已消费条目不被重新创建；待执行条目的编辑遵循来源顺序，后续读取显示来源结果，不虚构版本号保证。 | 原生一次性会话流程，加确定性适配器测试 | [场景](specs/pending-queue/spec.md#ac-06) |
| AC-07 | 重放与不确定变更：相同命令最多调用一次原生接口；变更载荷返回 payload_digest_mismatch；回执丢失结果保持 unknown，不重放来源变更。 | 单元测试，加协议和全链路集成测试 | [场景](specs/pending-queue/spec.md#ac-07) |
| AC-08 | 中断保留待执行工作：中断回执不宣称队列已清空；新读取展示剩余工作，每次撤回分别确认。 | 原生一次性会话流程，加确定性适配器测试 | [场景](specs/pending-queue/spec.md#ac-08) |
| AC-09 | 授权与私有内容：未授权请求不读取或变更 DSH；运行证据仅包含有界身份、结果和汇总数量。 | 单元测试，加协议和全链路集成测试 | [场景](specs/pending-queue/spec.md#ac-09) |
| AC-10 | 能力与变更边界：请求按情况以 unsupported_capability、conflict 或 invalid_argument 失败；不移除、重写或提升任何条目。 | 原生一次性会话流程，加确定性适配器测试 | [场景](specs/pending-queue/spec.md#ac-10) |
| AC-11 | 队列响应和编辑边界：操作返回 payload_too_large；不将部分队列作为完整队列展示，无效编辑不改变来源工作。 | 单元测试，加协议和全链路集成测试 | [场景](specs/pending-queue/spec.md#ac-11) |
| AC-12 | 重连后的新鲜状态：旧数据标记为过期或不可用；即使刷新失败，已确认变更仍保持确认，后续新读取替换旧视图。 | 单元测试，加协议和全链路集成测试 | [场景](specs/pending-queue/spec.md#ac-12) |

## 风险与权衡

- [被动状态规格未完整表达队列条目归属] → 启用条目变更前，使用一次性会话验证原生位置、来源和支持内容规则。若无法通过适配器拥有的原生 API 确认安全归属，则保持只读；不根据调用方提交的 ID 推断权限。
- [编辑与消费、移除与引导竞争] → 以来源执行顺序为准；不承诺 CAS 或提升失败的原子性。变更可能已经开始时保留 unknown。
- [冷状态行为] → 原生状态类型约定没有实时 Agent 时 queue=null；公布被动读取能力前，验证实际查询不会恢复工作。
- [内容体积与敏感内容] → 复用既有消息块白名单、明确的完整响应上限和临时明文；内容不进入日志及元数据证据。
- [并发协议变更] → 在实现阶段协调其他变更后分配增量字段，本方案不提前指定线协议编号。

## 迁移计划

Implement shared command invocation identity with the first mutation consumer. Queue reading is independent of sending; optional delivery_id correlation does not block reading or editing. Then add query/capability types，然后实现变更及 Core/CLI。启用客户端命令前先部署 Host 支持；不支持的 Host 明确失败，既有客户端不变。回滚时先撤回能力公布和客户端命令，再回滚来源代码；保留命令日志。不需要迁移原生队列或持久化队列数据。

## Agent 执行说明

真实 DSH 发布验证使用一次性目录和会话，以及不包含隐私的合成文本：在 queued/steering/context 位置加入条目，检查原生来源和关联，执行被动冷读取，对符合条件的条目逐项编辑、移除和提升，强制触发消费及阶段竞争，中断并读取剩余工作，再重连和恢复结果。仅记录有界身份、位置、数量及成功或失败；不保存指令正文、工具参数或凭据。若行为与目标契约不符，在启用相关操作前停止并修订方案。

未来检查：`./repo proto --check`；`./repo check --project core --project cli --project proto`；`./repo test --project core --project cli --project proto --project host --project dsh --coverage`；`./repo test --suite integration --group protocol`；完整的 `./repo test --suite integration`。Node 清单注册了测试，但没有 check/build 脚本，因此不选择不支持的 Node 检查。每个受影响包或 crate 要求 90% 单元行覆盖率。规划期间未运行实现测试或原生验证流程。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.

## Native parity review evidence

Controller state()/queueItems()/updateQueue()/cancel() distinguish cold null from live [], preserve native item IDs/placements, replace the whole item content on edit, require running plus next-turn placement for queue-item promotion, and pass keepInbox=true on interrupt. Direct prompt steer has different idle behavior. Privileged/context entries remain read-only under WhaleRein authorization, not because native updateQueue enforces that restriction.

Evidence is from the locally installed DSH 0.2.2-rc.1 packages, reviewed read-only. Native runtime/integration acceptance has not run. Existing transport authorization, command deduplication, encryption, privacy and uncertain-outcome recovery remain intentional remote-adapter boundaries.

## Capability acceptance gates

These are logical operation gates, not mandatory new wire flags. Follow the [evidence contract](../../README.md#acceptance-evidence-contract): all listed AC variants need current evidence, and source behavior needs the mapped native procedures. A flag covering several operations is enabled only when all its operation gates pass. Negative unsupported-capability tests passing do not enable an unverified operation. Existing shipped operations remain governed by their existing support; report any regression separately.

| Operation | Required ACs (all mandatory variants) | Required native evidence subset |
|---|---|---|
| Passive queue read | AC-01, AC-02, AC-09, AC-11, AC-12 | AC-01, AC-02, AC-12 |
| Text replacement | AC-03, AC-06–AC-12 | AC-03, AC-06, AC-08, AC-10, AC-12 |
| Item withdrawal | AC-04, AC-06–AC-12 | AC-04, AC-06, AC-08, AC-10, AC-12 |
| Queued-item promotion | AC-05–AC-12 | AC-05, AC-06, AC-08, AC-10, AC-12 |

Record each operation as enabled or disabled, its gate evidence, blockers and tested native version in the delivery summary. Keep any disabled path unavailable and retain FAIL/BLOCKED/NOT RUN results. A partial rollout is not full acceptance. The [combined workflow cases](../add-dsh-session-creation/design.md#combined-workflow-acceptance) qualify the integrated feature set separately and do not block independent delivery of verified operations.
