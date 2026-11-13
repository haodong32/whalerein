# 模型与推理强度切换设计

## Decision Review

Confirmed rows record only the choices listed below. Other draft choices still need evidence or review.

| ID | Decision / recommendation | Alternatives / tradeoffs | Status | Basis | Affected acceptance |
|---|---|---|---|---|---|
| D-REL-01 | Deliver verified capabilities independently; keep unverified capabilities disabled and report them. Dependencies still apply. | Deliver all seven together, or wait for all native probes. | CONFIRMED | User reply in this chat on 2026-11-11. This confirms delivery policy, not runtime success or blanket approval of the plan. | All ACs |
| D-MODEL-01 | Follow native DSH selection, including the background default-save attempt; display its scope. | Session-only override or synchronous default save would change native behavior. | CONFIRMED | User: follow DSH behavior. | AC-04, AC-11, AC-14 |
| D-MODEL-02 | Use native admission without extra idle/empty-queue/no-input restrictions or an acknowledgement flag. | Earlier draft added conservative preconditions and explicit acknowledgement. | CONFIRMED | Explicit user choice in this chat. AC-04, AC-05, and AC-07 were revised accordingly. | AC-04–AC-07 |
| D-LIMITS-01 | Reuse verified DSH and existing protocol limits first. Investigate and separately justify any necessary additional bound; seek review when it changes observable behavior. | Earlier draft numbers were not established native constraints. | CONFIRMED | Explicit user choice in this chat; exact source limits still need evidence. | AC-13 |

## 当前背景

动机见[方案](proposal.md)。现有执行源职责与恢复语义见 [Harness 契约](../../../docs/architecture/harness-contract.md)及 [Core 架构](../../../docs/architecture/core.md)。

已检查 `host/runtime/src/contracts/source.js`、`host/runtime/src/commands/service.js`、`host/dsh/src/adapter/{base,real,conversation}.js`、`core/src/api/core.rs`、`apps/cli/src/{cli,run,output}.rs` 和 `proto/v1/whalerein.proto`。目前这些位置没有模型操作。`host/runtime/test/{commands,source-contract}.test.js`、`host/dsh/test/real-adapter.test.js` 及 Core 命令恢复测试提供了对应验证入口。

本机原生代码证据位于 `/Users/haodong/.nvm/versions/node/v25.9.0/lib/node_modules/@deepseek-ai/dsh/node_modules/@deepseek-ai/`：`dsh-api-session-controller/lib/index.js:473-518` 构建模型目录，`:704-719` 选择模型后异步保存默认配置。`dsh-agent/lib/types/model-selection.d.ts` 定义被动 `modelSelection` 投影的 `pending` 和 `lastUsed` 字段。`sessionController.projections` 支持冷会话投影读取。这些均为源码检查，不是发布验收证据。

## 目标与非目标

**Goal:** Provide typed model operations through the existing secure transport, preserve native acceptance conditions, and accurately report receipts, passive reads, and default-configuration effects.

**非目标：** 凭据管理、自动回退、自动回滚、在 Core 中解释原生 JSON、建立第二个配置存储，或绕过原生控制器实现仅当前会话切换。本变更不保证模型任务开始执行，也不增加超出原生控制器的准入或原子性承诺。

## 架构评审

| 边界 | 职责与依赖方向 |
| --- | --- |
| CLI → Core | Parse `models --host`, `model get --session`, and `model set --session --provider --model [--effort]`. Display native effect scope in human and JSON output; no additional acknowledgement argument is required. |
| Core → Host | 使用类型化 `list_models`、`read_session_model`、`select_model` 载荷、能力检查及现有命令日志和结果恢复；新增 API，不改变现有发送方法签名。 |
| Host → SourceAdapter | 增加默认不支持的目录和被动选择读取方法；通过 `execute(sessionId, command, context?)` 及现有持久化命令流程执行选择。 |
| DSH adapter → native services | Only the adapter calls `modelCatalog`, `projections`, and `selectModel`. Normalize reasoning-effort IDs, preserve native admission, sanitize errors, and bound read responses. Add no idle, empty-queue, pending-input, or acknowledgement gate. |
| Access | 保留不解释业务内容的中继、授权和保留策略，不解释模型或提供方。 |
| 状态归属 | DSH 拥有模型选择和默认值。Host 存储命令结果/回执，不保存模型配置副本。Core 提供新读取；首版不增加持久化模型缓存。 |

共享 Host 能力查询由 `add-dsh-session-creation` 负责。在其 Host 能力中增加独立的模型目录与选择读取标志，在会话能力中增加模型修改标志。缺失字段表示 false。各方案不分别预分配 protobuf 字段编号；实现依赖协调后统一分配并生成绑定。

## 关键决策

1. **Preserve native default effects (D-MODEL-01).** Use the native controller, including its background default-save attempt. Return the accepted selection, `effect_scope=session_and_source_default_attempt`, and `default_persistence=unconfirmed`. Display the effect without a separate acknowledgement bit. Bypassing the controller for session-only behavior or forcing synchronous default persistence would alter the chosen behavior.
2. **采用被动投影读取。** 返回归一化的待使用选择、最近使用选择，以及单独标注的执行源默认值，不调用 `resolveAgent`。已选模型不一定是最近使用模型。冷会话或没有记录时字段为空，不根据当前默认值推导虚构的持久化选择。
3. **Delegate admission to DSH (D-MODEL-02).** Remove the proposed idle, known-empty-queue, and no-pending-input restrictions. Invoke the supported native selection path after ordinary authorization, capability, and payload checks. Verify its actual admission behavior with controlled fixtures and native evidence, including busy and cold sessions; do not promise that every state is accepted. The native path may restore a cold Agent, unlike passive selection reads. Do not introduce a lock claiming to serialize external DSH callers.
4. **Preserve uncertainty.** Validate payload shape, then let the native controller validate routes and effort. Proven native validation failures before selection may be definite failures; unexpected controller errors retain uncertainty. Later local changes do not invalidate a receipt for an earlier invocation.
5. **Use verified resource bounds (D-LIMITS-01).** Derive encoded-response and admission budgets from the native services and existing transport contract. Account for complete envelope overhead. Record exact evidence and boundary tests before enabling reads. Propose and justify any necessary additional count, size, or concurrency limit separately. Keep sanitized partial-provider failures and native route validation.
6. **不建立实时配置副本。** 通过新查询观测本地变化，不专门为模型增加 watch 投影，因此首版不依赖 `add-dsh-session-wait` 新增的运行状态字段。适配器仍可在自身边界内读取原生接受状态。

## 验收评审

每行均链接到权威场景。任务中的所有结果初始为 `NOT RUN`；源码检查和 OpenSpec 校验不能使这些场景通过验收。

| 验收编号 | 场景与预期结果 | 验证方式 | 规格场景 |
| --- | --- | --- | --- |
| AC-01 | 能力缺失或不完整时仅禁用不支持的模型操作 | 协议默认值和 Core/CLI 单元夹具 | [AC-01](specs/session-model-selection/spec.md#ac-01) |
| AC-02 | 部分目录失败时保留可用路由，并清理失败提供方信息 | 适配器目录夹具与 CLI JSON 集成 | [AC-02](specs/session-model-selection/spec.md#ac-02) |
| AC-03 | 冷读取不激活会话，明确缺少记录并标注默认值 | 原生上下文调用监视单元测试；真实冷会话探测 | [AC-03](specs/session-model-selection/spec.md#ac-03) |
| AC-04 | Report native acceptance and its default-save attempt accurately | Adapter receipt tests and controlled native selection flow | [AC-04](specs/session-model-selection/spec.md#ac-04) |
| AC-05 | No extra acknowledgement flag or acknowledgement conflict | CLI/Core/Host tests and native invocation monitoring | [AC-05](specs/session-model-selection/spec.md#ac-05) |
| AC-06 | 路由过期或档位无效时拒绝，不回退 | 原生校验夹具与真实不可用路由探测 | [AC-06](specs/session-model-selection/spec.md#ac-06) |
| AC-07 | Native admission decides busy, queued, input-waiting, and unknown-state outcomes | Adapter admission fixtures and controlled native checks | [AC-07](specs/session-model-selection/spec.md#ac-07) |
| AC-08 | 授权撤销后，在原生访问前阻止读取和修改 | 完整集成中的撤销设备流程 | [AC-08](specs/session-model-selection/spec.md#ac-08) |
| AC-09 | 重复命令返回存储结果，摘要变化被拒绝 | Host 命令故障测试和 Core 恢复测试 | [AC-09](specs/session-model-selection/spec.md#ac-09) |
| AC-10 | 响应或存储丢失时保留不确定性，不二次调用 | 命令日志故障及重连集成测试 | [AC-10](specs/session-model-selection/spec.md#ac-10) |
| AC-11 | 读取可见后续本地选择，原回执保持稳定 | 执行源夹具与真实本地/远程序列 | [AC-11](specs/session-model-selection/spec.md#ac-11) |
| AC-12 | 离线时不延迟修改，不伪造新目录 | Core 离线/重连测试 | [AC-12](specs/session-model-selection/spec.md#ac-12) |
| AC-13 | 经查证的原生／既有协议预算边界和归一化失败不泄密 | 编码大小边界测试及使用不透明哨兵值的诊断断言 | [AC-13](specs/session-model-selection/spec.md#ac-13) |
| AC-14 | 后台保存失败不重放，也不否定已接受选择 | 原生上下文保存失败夹具；可安全注入时进行原生探测 | [AC-14](specs/session-model-selection/spec.md#ac-14) |

## 风险与取舍

- 原生内部 API 变化 → 启用模型能力前核实受支持安装版本；缺少方法或投影时禁用具体能力，不猜测读取路径。
- Native default effects can affect later sessions: display the scope and report actual receipts; do not automatically restore defaults and overwrite local changes.
- Local changes can race selection: retain native outcomes and test concurrency; do not claim session-only scope or atomic next-call behavior.
- DSH 自身记录默认配置保存失败 → WhaleRein 不将这些错误转发到自己的诊断。本方案约束 WhaleRein 日志，不认证原生日志策略。
- 当前没有真实执行源验收证据 → 启用能力以完成下述定向探测为前提；夹具不能认证原生持久化或并发行为。

## 迁移与回滚

在共享 Host 能力契约之后实现。采用增量消息和能力，缺省不支持；更新 Host 和适配器后再启用新客户端操作。保留旧载荷和 CLI 命令。回滚时禁用模型能力，保留已接受命令日志以供恢复，不隐式回滚用户的模型选择。不计划数据库迁移或新增基础设施。

## Agent 执行说明

Before enabling native capabilities, use a dedicated configuration to verify passive cold `modelSelection` reads, native model-selection admission across idle, running, input-waiting, queued, unknown-state, and cold cases, the default-save attempt, and reads after later local changes. Unlike passive reads, selection follows native restoration behavior. Record supported combinations and exact versions in `.local-docs/acceptance/`. If a required native contract cannot be verified, keep only the affected capability disabled and report it; do not weaken its acceptance criteria.

计划检查遵循[测试要求](../../../docs/testing.md)：Core/CLI 定向静态检查，Host/DSH/Core/CLI 单元覆盖率（受影响包至少 90%），生成协议检查、协议集成，以及覆盖跨组件和恢复行为的完整集成套件。详细命令和证据位置见[任务](tasks.md)。这次仅编写方案，没有运行应用检查。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.

## Native parity review evidence

Controller selectModel() resolves/restores the Agent, serializes with image admission, resolves native call configuration, records selection and attempts saveSelection in the background. Passive projection reads remain non-activating. Preserve absent effort and the actual returned selected value; do not fill missing choices or add idle/queue gates. Source facts do not establish default persistence or live concurrency acceptance.

Evidence is from the locally installed DSH 0.2.2-rc.1 packages, reviewed read-only. Native runtime/integration acceptance has not run. Existing transport authorization, command deduplication, encryption, privacy and uncertain-outcome recovery remain intentional remote-adapter boundaries.

## Capability acceptance gates

These are logical operation gates, not mandatory new wire flags. Follow the [evidence contract](../../README.md#acceptance-evidence-contract): all listed AC variants need current evidence, and source behavior needs the mapped native procedures. A flag covering several operations is enabled only when all its operation gates pass. Negative unsupported-capability tests passing do not enable an unverified operation. Existing shipped operations remain governed by their existing support; report any regression separately.

| Operation | Required ACs (all mandatory variants) | Required native evidence subset |
|---|---|---|
| Model catalog | AC-01, AC-02, AC-08, AC-12, AC-13 | AC-02 |
| Passive selection read | AC-01, AC-03, AC-08, AC-11–AC-13 | AC-03, AC-11 |
| Native model selection | AC-01, AC-04–AC-14 | AC-04, AC-06, AC-07, AC-11, AC-14 |

Record each operation as enabled or disabled, its gate evidence, blockers and tested native version in the delivery summary. Keep any disabled path unavailable and retain FAIL/BLOCKED/NOT RUN results. A partial rollout is not full acceptance. The [combined workflow cases](../add-dsh-session-creation/design.md#combined-workflow-acceptance) qualify the integrated feature set separately and do not block independent delivery of verified operations.
