# 设计方案

## Decision Review

Confirmed rows record only the choices listed below. Other draft choices still need evidence or review.

| ID | Decision / recommendation | Alternatives / tradeoffs | Status | Basis | Affected acceptance |
|---|---|---|---|---|---|
| D-REL-01 | Deliver verified capabilities independently; keep unverified capabilities disabled and report them. Dependencies still apply. | Deliver all seven together, or wait for all native probes. | CONFIRMED | User reply in this chat on 2026-11-11. This confirms delivery policy, not runtime success or blanket approval of the plan. | All ACs |
| D-QUEST-01 | Follow verified native question ownership and supported scope, rather than a blanket root-only product restriction. | Artificially restrict all child-lineage sessions, or bypass native ownership. | CONFIRMED | User requested native scope. Installed DSH assertLiveRoot rejects currently delegated Agents; a lineage-bearing session resumed as a live runtime root can ask normally. Runtime verification remains outstanding. | AC-01, AC-03, AC-04, AC-06, AC-08–AC-10 |
| D-LIMITS-01 | Reuse verified DSH and existing protocol limits first. Investigate and separately justify any necessary additional bound; seek review when it changes observable behavior. | Earlier draft numbers were not established native constraints. | CONFIRMED | Explicit user choice in this chat; exact source limits still need evidence. | AC-11 |

## 背景

参见[提案](proposal.md)。适配器当前仅注册 `approval/request`；`SourceAdapter.execute()` 和 Host 日志保留控制结果的不确定性。已安装的 `dsh-user-questions/lib/types/types.d.ts:91-174` 区分 open/continued 批次并定义请求瀑布流；`lib/types/index.d.ts:34-99` 提供 continued 回复的 `answer()`，以及活动等待认领的 `attachWait()`。`lib/index.js:560-595` 校验 continued 批次身份、抑制重复入队，并以 steering 方式提交带标记的回复；权威结算在准入后发生。这些仅是静态源码事实，不能证明本地/远程 open 仲裁安全。

Shares normalized input-state types with `add-dsh-session-wait`, but question read/answer does not depend on the wait operation being implemented or enabled. 可选执行调用身份与 `add-dsh-message-delivery` 对齐，不依赖队列编辑。实施前协调两份方案。

## 目标与非目标

**目标：** 通过显式守卫提交人工回答，由数据源拥有生命周期，保证一次准入结果；重连后重建可读待处理状态。

**非目标：** 不自动决策，不发送兜底普通消息，不透传原生 JSON，不持久化明文问题存储，也不替换审批策略。Follow verified native runtime ownership (D-QUEST-01): currently delegated Agents remain unsupported where DSH rejects them, while a lineage-bearing session resumed as a live runtime root is not rejected merely because it has a parent. Verify open and continued paths separately before enabling them.

## 架构评审

DSH 负责问题定义、阶段转换、校验和结算。仅 DSH 使用的问题桥接器归一化投影/活动请求数据，将不透明 ID 映射回原生调用和精确活动根 Agent。Host 负责传输授权、摘要绑定的命令准入与结果日志，不能制造原生原子性。Core 提供带类型批次/回答，校验作用域、纪元和守卫；CLI 读取显式结构化输入。Access 保持不透明，Core 唯一注入边界仍是 SecretStore。临时桥接句柄不构成持久化的第二份内容日志。

## 设计决策

1. **问题控制与审批分开。** 增加归一化 `QuestionBatch`/`QuestionItem`/`QuestionAnswer` 类型、原生答案字段与批次 ID 完整性守卫、`open`/`continued`/`reply_queued` 展示，以及数据源纪元/版本/摘要。使用限定于主机/会话的不透明稳定批次 ID，不暴露原生事件或运行时对象。新增每会话 `questions_read`、`questions_answer_open`、`questions_answer_continued` 标记，缺失时均为 false。既有审批 API 和摘要定义不变。
2. **原生路径保持阶段独立。** 从原生 `userQuestions` 投影读取有时间限制的待处理批次，在数据源支持时从原生句柄读取活动 open 问题。continued 回答在解析受支持根 Agent 后调用服务的 `answer()`；accepted=true 表示 `reply_queued`，而非已结算。open 回答使用组合的原生回答器/请求生命周期和等待认领，不调用 continued `answer()`，也不合成聊天。若可证明阶段/版本已变化，则在调用前拒绝过期提交。不跨路径使旧回答“成功”。
3. **原生仲裁是能力启用门槛。** 桥接器必须通过原生单次结算和认领生命周期，与既有回答器协作。不得安装一个前置监听器，无限期抢走本地 UI 的全部请求。启用 open 回答前，必须使用专门的真实 DSH 夹具证明本地优先/远程优先并发结算、有时限的认领释放、断连和取消。若现有原生边界无法在不抢占本地输入的情况下满足要求，则保持 open 回答关闭、报告阻塞，并将该路径修订提交评审；不得用虚构的 Host 锁或仅摘要检查替代。continued 能力有独立的重复准入/归属验证门槛。只读发现可独立保持可用。这是明确的实施/发布门槛，不声称当前已安全。
4. **一个持久化命令，一次原生调用。** 回答控制经过既有日志/摘要处理，以及 `execute(sessionId, command, optional invocationContext)`。复用从持久化命令准入派生的稳定、与数据源无关的调用身份；原生映射限制在适配器中。重复命令先返回已存储结果，再进行待处理状态预检。已证实的调用前失败是确定失败；调用后任何不确定性均保持 unknown，包括回执丢失或日志失败。重启后绝不自动重放回答。原生 continued 重复抑制另行守卫独立提交的本地/远程重复批次。
5. **新鲜守卫和有界内容。** 根据规范化问题身份、阶段、允许的回答模式及原生纪元派生版本/摘要；重连后从权威状态重建。仅设备重连时，若精确的活动原生句柄仍存在，且新作用域/阶段/版本守卫校验通过，可重新暴露 open 请求。数据源或适配器重启后，不得根据投影重建已消失的 Promise；仅当原生路径独立确认当前可回答性时才暴露批次。绝不只持久化投影，再凭空创建活动 open Promise。Question counts, option counts, and payload budgets follow verified native and existing protocol constraints (D-LIMITS-01); remove the draft 16/32/64 KiB limits. Inspect complete response/envelope budgets and native validation before setting fixtures; any necessary additional bound needs separate evidence and review. Keep bounded list pages/cursors without silent omission. 超大批次在列表中保留有界的不可用元数据，不允许部分回答。请求其完整内容或提交超大回答时返回 payload_too_large，不调用部分回答。CLI 从文件或标准输入接受一个完整 JSON 批次，显式指定会话/批次/版本/摘要，错误不回显输入。

## 验收评审

| AC 编号 | 场景与预期结果 | 验证方法 | 来源场景 |
|---|---|---|---|
| AC-01 | 列出原生问题批次：响应仅按数据源顺序包含该会话当前待处理批次，不含提供方重放载荷；审批请求保持独立类型 | 适配器投影单元测试和带类型的协议夹具 | [AC-01](specs/user-questions/spec.md#ac-01) |
| AC-02 | 格式错误或不完整的回答：操作返回 invalid_argument，不结算原生请求，也不将回复入队 | Host/适配器校验单元用例 | [AC-02](specs/user-questions/spec.md#ac-02) |
| AC-03 | 回答 open 问题：一个原生请求使用所提交回答完成结算，响应标识该批次；客户端不声称后续 Agent 工作已经完成 | 原生 open 请求夹具和 FakeDSH 命令集成测试 | [AC-03](specs/user-questions/spec.md#ac-03) |
| AC-04 | 前台窗口结束后回答：回执返回 reply_queued；仅在权威数据源状态确认准入后，批次才结算；不伪造重复普通聊天消息 | 原生 continued 回复夹具和适配器集成测试 | [AC-04](specs/user-questions/spec.md#ac-04) |
| AC-05 | 显示过期或阶段变化：在能够证明结果时，回答在原生变更前返回 conflict 或 question_unavailable；用户必须刷新并针对新阶段显式重新提交 | 适配器状态转换/竞争测试和原生截止时间夹具 | [AC-05](specs/user-questions/spec.md#ac-05) |
| AC-06 | 本地和远程同时回答：仅有一个回答被原生权威接纳；失败方不发送兜底消息、不覆盖胜出回答，也不抢占未来本地请求 | 真实 DSH 双回答与取消验收流程 | [AC-06](specs/user-questions/spec.md#ac-06) |
| AC-07 | 重复命令或回执丢失：相同摘要的重复请求恢复已存储结果，不二次调用；不同摘要复用被拒绝；不确定副作用保持 unknown，由显式结果恢复处理 | Host 命令日志测试和跨组件响应丢失集成测试 | [AC-07](specs/user-questions/spec.md#ac-07) |
| AC-08 | 错误会话或已撤销设备：不暴露问题内容或原生回答；返回稳定的授权、未找到或不支持结果，不跨作用域泄露批次存在性 | Host 作用域/撤销授权测试和原生归属夹具 | [AC-08](specs/user-questions/spec.md#ac-08) |
| AC-09 | 问题期间重连或重启：仅设备重连时，在重新校验作用域、阶段、版本与活动句柄后，可重新暴露 open 批次；数据源/适配器重启绝不根据缓存文本重建已消失的 open Promise，仅暴露通过受支持原生路径独立确认仍可回答的批次 | 适配器重启和 Core 基线集成测试 | [AC-09](specs/user-questions/spec.md#ac-09) |
| AC-10 | 未验证或旧版数据源：对应操作返回 unsupported_capability，不拦截原生问题；旧审批行为保持不变 | 协议默认值和适配器委托测试 | [AC-10](specs/user-questions/spec.md#ac-10) |
| AC-11 | 私有或超限的问题批次：列表对超限批次仅返回有界的不可用元数据；完整内容读取和超限回答返回 payload_too_large，不部分接纳回答；正常及错误诊断只含允许的元数据，不含问题或回答文本 | 载荷边界测试、密文检查和脱敏标记流程 | [AC-11](specs/user-questions/spec.md#ac-11) |
| AC-12 | 人工可读与 JSON CLI 流程：人工可读/JSON 输出区分 open 结算与 reply_queued；非法输入错误可供用户处理；拒绝离线提交，不将其输入当作新聊天消息 | CLI 解析/输出测试和 CLI 到 FakeDSH 的集成测试 | [AC-12](specs/user-questions/spec.md#ac-12) |
| AC-13 | Preserve native question context: show the complete available decision context and return exact selected labels/custom text; distinguish projection-only discoverability from active answerability, never infer approval by position, and leave unverified open-provider arbitration disabled | Typed protocol/CLI fixtures and real scoped provider/waterfall checks | [Scenario](specs/user-questions/spec.md#ac-13) |
| AC-14 | Release open-question claims: release or expire the exact claim; preserve native local/provider handling of still-active or subsequent questions, reject stale submissions without a second settlement, and prevent the old handle from claiming later requests. Keep remote open capability disabled if this cannot be verified | Real DSH scoped provider/attachWait procedures plus lifecycle and stale-handle fixtures | [Scenario](specs/user-questions/spec.md#ac-14) |

## 风险与取舍

- [open 原生仲裁尚未经过运行验证] → 在准确的验证门槛通过前，保持其能力关闭；continued 路径不能替代。
- [continued 回复已入队但随后被移除] → 分别报告入队准入；以权威待处理/已结算状态为准，unknown 命令通过结果恢复处理，不重放。
- [问题文本敏感] → 仅作为加密协议内容；日志和诊断仅含元数据；脱敏夹具使用合成标记，不转储载荷。
- [设备重连与原生重启不同] → 仅在新守卫校验后复用精确存活的活动句柄；数据源/适配器重启后，绝不根据缓存文本重建 open 回答路径。
- [原生根归属变化] → 调用时再次检查精确活动根；拒绝过期/被持有子 Agent 访问，不挂载未经授权的新回答器。

## 迁移计划

协调并行方案后增加协议字段和命令变体，通过 `./repo proto` 重新生成，并一并更新 Host 校验/路由和 Core/CLI。能力标记默认为 false，因此旧对端保持审批/tail 行为，并明确拒绝新操作。不新增持久化问题数据库或 API 监听器。回滚时清理桥接句柄、关闭能力公告，将原生问题继续委托给其他回答器，同时保留命令结果。

## Agent 执行细节

先使用合成原生根问题、短前台等待窗口和原生本地 UI 回答器完成上述数据源验证门槛，将脱敏时序/状态观察记录在 `.local-docs/acceptance/`。检查 `answer()` 返回 false、BAD_ANSWER 和 REPLY_QUEUED 映射、open→continued 转换、过期根以及 DSH 重载。若验证失败，停止该路径；改变范围或验收前先提交评审。随后实现归一化类型/限制、桥接器、Host 控制、Core API 和 CLI。验证命令与全部最新证据要求见 tasks.md。规划期间未执行真实集成或验收。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.

## Continued eligibility and queue observation

A continued timed projection is an opportunity to submit only when the same live runtime root owns the exact live Session, both native inbox lists are available and the native answer service is present. A matching `user-question-reply` visible in either inbox is represented as `reply_queued` and not answerable; this observation changes the version/digest guard. The native service may also retain a private in-flight reply after it leaves the inbox. That private state is not inspected or reconstructed: `answerable` is a sampled opportunity, not a reservation or promise of native admission. DSH `REPLY_QUEUED` is a proven pre-admission conflict, and other unclassified native exceptions remain unknown. The capability remains disabled until all mapped acceptance gates pass.

## Native question metadata and provider boundary

Native AskUserQuestionItem carries id, question, detail, header, option labels/descriptions, multiSelect and optional typed plan-review/approval intent. Answer items carry selected labels and optional custom text. Preserve these through normalized typed fields; never replace answer labels with display indices or infer permission grants from option order. Unknown presentation intent falls back to complete generic context, not automatic approval. Do not invent a separate allow-free-text capability or additional selected-label/count restrictions. On DSH 0.2.2-rc.1, isolated continued and open service probes accepted unoffered labels, repeated labels, multiple labels for a single-select item and custom-only responses. Preserve the submitted string arrays and optional custom text; keep exact per-question ID and field-type guards. Native intent remains presentation context, never an inferred permission grant.

The native timed projection does not contain legacy indefinite calls and does not preserve every live-request intent field. Supplement it only with legitimately observed live provider handles; never reconstruct missing permission detail or an active Promise from history. Native user-questions/request is a scoped answerer waterfall: returning a result claims the request, next() delegates. attachWait holds an existing timed wait; it is not a public listing or arbitrary open-answer RPC. Existing runtime ownership and local UI providers remain authoritative. Prove cooperative provider/claim behavior before enabling remote open answers; a Host lock or unconditional front listener cannot supply native arbitration. continued answer() is a separate native path and remains independently releasable.

## Native parity review evidence

dsh-user-questions/lib/types/types.d.ts defines native question/answer fields and scoped waterfall behavior. lib/index.js assertLiveRoot(), answer(), attachWait() and askTimed() distinguish ownership, queued late replies and timed claims. Projection-only timed questions are not arbitrary live answer handles. Preserve available intent/detail from live requests, distinguish unsupported legacy discovery, and verify native provider cooperation before enabling open answers.

Evidence is from the locally installed DSH 0.2.2-rc.1 packages, reviewed read-only. Native runtime/integration acceptance has not run. Existing transport authorization, command deduplication, encryption, privacy and uncertain-outcome recovery remain intentional remote-adapter boundaries.

## Capability acceptance gates

These are logical operation gates, not mandatory new wire flags. Follow the [evidence contract](../../README.md#acceptance-evidence-contract): all listed AC variants need current evidence, and source behavior needs the mapped native procedures. A flag covering several operations is enabled only when all its operation gates pass. Negative unsupported-capability tests passing do not enable an unverified operation. Existing shipped operations remain governed by their existing support; report any regression separately.

| Operation | Required ACs (all mandatory variants) | Required native evidence subset |
|---|---|---|
| Question discovery | AC-01, AC-08–AC-11, AC-13 | AC-01, AC-08–AC-10, AC-13 |
| Continued answer | AC-01, AC-02, AC-04–AC-13 | AC-01, AC-04–AC-06, AC-08–AC-10, AC-13 |
| Open answer | AC-01–AC-03, AC-05–AC-14 | AC-01, AC-03, AC-05, AC-06, AC-08–AC-10, AC-13, AC-14 |

Record each operation as enabled or disabled, its gate evidence, blockers and tested native version in the delivery summary. Keep any disabled path unavailable and retain FAIL/BLOCKED/NOT RUN results. A partial rollout is not full acceptance. The [combined workflow cases](../add-dsh-session-creation/design.md#combined-workflow-acceptance) qualify the integrated feature set separately and do not block independent delivery of verified operations.
