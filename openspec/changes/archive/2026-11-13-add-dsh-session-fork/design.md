# 设计方案

## Decision Review

Confirmed rows record only the choices listed below. Other draft choices still need evidence or review.

| ID | Decision / recommendation | Alternatives / tradeoffs | Status | Basis | Affected acceptance |
|---|---|---|---|---|---|
| D-REL-01 | Deliver verified capabilities independently; keep unverified capabilities disabled and report them. Dependencies still apply. | Deliver all seven together, or wait for all native probes. | CONFIRMED | User reply in this chat on 2026-11-11. This confirms delivery policy, not runtime success or blanket approval of the plan. | All ACs |
| D-FORK-01 | First version forks the latest complete turn, shares the working directory, and does not automatically run the child. State the shared-directory effect explicitly. | Arbitrary historical cuts or worktree isolation would expand scope. | CONFIRMED | Explicit user choice in this chat. | AC-01, AC-02, AC-04, AC-11 |

## 上下文

见[提案](proposal.md)与[规范](specs/session-fork/spec.md)。本变更沿用 `add-dsh-session-creation` 的 HostCapabilities 和归一化生命周期回执约定，不增加新的主机执行接口。分叉是通过 `SourceAdapter.execute(sessionId, command)` 执行的会话范围操作，沿用同一持久化命令日志。

本机安装的 DSH 0.2.2-rc.1 控制器 `lib/index.js:756-811` 观察可冷读的来源，将省略的 atSeq 解析为最近完整轮次前缀，构造原生种子、创建新子会话，再绑定到工作区。`session/fork-unavailable` 可标识没有完整轮次的来源。后续工作区错误包含结构化子身份。尚未运行真实来源检查。原生分叉当前自行解析模型／预设配置；本提案不保证复制父会话的实际模型选择。

## 目标与非目标

**目标：** 提供明确的完整轮次分叉命令、独立缓存／订阅身份，以及保持源会话不变的恢复机制。

**非目标：** 不支持任意历史切点、Core 解释原生序号、提供方重放负载导出、文件／worktree 隔离、自动继续子会话、覆盖模型／预设，或中断源会话。

## 设计决策

### 架构评审

| 层 | 职责与状态所有权 |
|---|---|
| CLI | 针对当前选定且已配对 Host 解析 `session-fork --session ID`；格式化子／父身份、领域处置状态和共享目录说明。不提供 atSeq 选项。 |
| Core | 携带完整 Host／会话身份提交带类型分叉，沿用既有控制保留不确定命令恢复。读取子会话时获取新基线，绝不复制源缓存游标／Attempt 状态。 |
| 共享 Host | 通过既有命令日志接纳 `fork_session`，路由到 `execute(sourceSessionId, {fork_session})`，按创建约定编码生命周期回执。 |
| DSH 适配器 | 仅在原生前提条件满足时宣告分叉能力；以源 ID 调用原生分叉，不传 atSeq；归一化安全回执／错误证据。父身份来自调用目标，不捏造原生历史字段。持久化编码回执仅包含父／子身份、操作／处置状态和稳定警告码；实际目录由独立权威摘要读取获取，绝不持久化到明文 Host 命令结果。 |
| DSH | 拥有完整轮次边界选择、种子构造、子会话内容、工作区成员关系和已配置执行行为。 |
| Access | 沿用授权校验和不透明路由，不持久化父／子内容或生命周期。 |

沿用创建建立的共享 HostCapabilities 查询，在其中增加分叉支持；来源限制需要时另增每会话 SourceCapabilities 分叉标志。缺失标志视为不支持。主机全局支持绝不覆盖每会话限制。协调全部七项方案前不预留 protobuf 字段编号。

### 边界与子会话身份

只发送源身份。原生代码在调用时选择当前最近完整前缀；预先读取历史会引入竞态，并迫使 Core 理解原生序号。后续运行中／排队的源任务不得作为可执行输入被子会话继承。分叉不取消源任务。没有完整轮次时返回 conflict，不能静默退化为创建空白子会话。能证明未发生变更时，对不存在的来源返回 not_found。

沿用创建的 `READY` 和 `READY_WITH_WARNING` 回执处置状态，携带已确认子身份和父身份。操作结果中的父身份就是请求来源；常规会话列表仅在原生提供父投影时使用，不合成历史事件。既有 `SessionSummary.parent_session_id` 不证明原生分叉会填充该投影，因此回执中的父身份对本次调用具有权威性，摘要谱系不构成额外验收承诺。不传输提供方重放数据。

### 恢复、缓存与部分结果

使用既有持久化身份／摘要，在原生调用前去重。重放返回相同子会话回执，而有意使用不同命令可创建独立子会话。未知的原生／结果持久化结果保持 unknown，不能根据目录、标题、列表顺序或事件数量推断子身份。子会话创建后的结构化工作区错误保留已知子身份，并明确绑定未完成。不得通过删除子会话补偿，也不得再次执行原生操作。

Core 在 `(host_id, child_session_id)` 下独立创建／获取子基线，保留父会话游标、视图、订阅及会话范围内的临时 Attempt 状态。适配器可保留既有共享 live_epoch 字符串；独立性来自完整 Host／会话／Attempt 身份，而非新建每会话 epoch 机制。父子会话可能显示带来源 ID 的继承 Messages，缓存命名空间必须防止碰撞。成功分叉后的读取失败影响同步，不改变已经确认的分叉结果。任何一方都不因存在同源分支而收到合成历史重置。

### 兼容性与发布

这是 v1 带类型负载和既有 SourceCapabilities 的增量扩展，旧字段／路由／主题不变。旧 Host 沿用创建的能力查询处理。不新增数据库会话表或服务。只为已验证完整轮次选择及待执行收件箱行为的运行时启用能力；回滚撤回分叉能力宣告，保留子会话及恢复记录。

## 验收评审

| AC 编号 | 场景与预期结果 | 验证方法 | 来源场景 |
|---|---|---|---|
| AC-01 | 分叉最近完整轮次：结果确认一个不同的子会话身份，标明源父会话，并允许通过子身份读取继承的完整上下文，不新增用户提示词 | 适配器／Host 单元测试与隔离全链路集成；真实 DSH 合成配置检查 | [AC-01](specs/session-fork/spec.md#ac-01) |
| AC-02 | 后续轮次运行中进行分叉：原生选定的完整前缀定义子会话；后续源任务不会由子会话自动执行，源会话不被中断或修改 | 适配器／Host 单元测试与隔离全链路集成；真实 DSH 合成配置检查 | [AC-02](specs/session-fork/spec.md#ac-02) |
| AC-03 | 拒绝没有完整轮次的来源：请求返回已证明发生在子会话变更前的稳定 not_found 或 conflict 失败，不创建空白替代子会话 | 适配器／Host 单元测试与隔离全链路集成 | [AC-03](specs/session-fork/spec.md#ac-03) |
| AC-04 | 拒绝任意历史切点：公开命令在变更前拒绝不支持的参数，绝不通过游标运算猜测切点 | 适配器／Host 单元测试与隔离全链路集成 | [AC-04](specs/session-fork/spec.md#ac-04) |
| AC-05 | 保持父子会话独立：源会话保留既有历史视图和订阅；子会话拥有独立基线、游标和会话范围内的临时 Attempt 状态；子会话更新绝不覆盖源会话缓存消息 | 适配器／Host 单元测试与隔离全链路集成 | [AC-05](specs/session-fork/spec.md#ac-05) |
| AC-06 | 拒绝不支持未授权和过期请求：不发生原生分叉；不支持时返回 unsupported_capability，未授权调用不披露源详情，既有控制仍可使用 | 适配器／Host 单元测试与隔离全链路集成 | [AC-06](specs/session-fork/spec.md#ac-06) |
| AC-07 | 报告工作区绑定警告：结果保留子／父身份并返回 READY_WITH_WARNING，标记绑定未完成，绝不删除子会话或重复分叉 | 适配器／Host 单元测试与隔离全链路集成；真实 DSH 合成配置检查 | [AC-07](specs/session-fork/spec.md#ac-07) |
| AC-08 | 变更后保留不确定性：结果保持 unknown；不返回猜测的子身份或确定未创建子会话的失败，恢复绝不再次执行分叉 | 适配器／Host 单元测试与隔离全链路集成 | [AC-08](specs/session-fork/spec.md#ac-08) |
| AC-09 | 跨并发和重启去重：同摘要重复请求重放原接纳状态／结果和已确认子身份，最多调用一次；拒绝改变摘要的请求 | 日志／编码故障注入、Core 恢复与重启集成 | [AC-09](specs/session-fork/spec.md#ac-09) |
| AC-10 | 允许有意创建独立分支：每次确认的操作返回独立子身份；原生来源为每次操作选择完整前缀，不跨命令合并 | 适配器／Host 单元测试与隔离全链路集成 | [AC-10](specs/session-fork/spec.md#ac-10) |
| AC-11 | 说明共享工作目录：已知时明确子会话实际目录，面向人的使用说明指出对话分叉不创建 worktree 或文件系统隔离，也不暗示覆盖模型或预设 | 适配器／Host 单元测试与隔离全链路集成 | [AC-11](specs/session-fork/spec.md#ac-11) |
| AC-12 | 保持离线和诊断边界：离线控制被拒绝，不排队执行变更；继承消息内容、工具参数、原生事件序号、凭据或原始原生错误不进入日志或测试证据；Host 持久化生命周期回执不包含 cwd、工作区标识或预设内容 | 适配器／Host 单元测试与隔离全链路集成 | [AC-12](specs/session-fork/spec.md#ac-12) |


## 风险与权衡

- 原生分叉边界逻辑退化时可能继承过期排队输入 → 真实来源验收使用已完成源轮次以及后续运行中／排队任务；失败时不宣告能力。
- 分叉对话共享实际工作目录 → 输出和使用说明明确不创建 worktree 或文件系统隔离。
- 子会话模型选择不一定等于父选择 → 不承诺复制模型，覆盖操作留给模型选择能力。
- 创建成功后工作区绑定失败 → 保留结构化子身份并返回警告；缺少可靠变更证据的错误保持 unknown。
- 继承内容内可能复用会话 ID → 保留完整 Host／会话缓存命名空间，创建子基线，不复制源游标。

## 迁移计划

1. Implement shared lifecycle/capability types with the first consumer; coordinate protocol and recovery changes without requiring creation to ship first.
2. 扩展带类型分叉请求／能力及模拟／真实适配器控制，再接入 Core 和 CLI。
3. 运行协议／全链路恢复检查，以及隔离真实 DSH 的空闲／冷／运行中来源、无完整轮次、排队任务、工作区绑定警告和重载持久化场景。
4. 只在已验证运行时启用分叉；回滚禁用能力，不删除父／子会话或命令日志。

## Agent 执行说明

原生证据必须区分完整轮次的语义内容与原生边界实现。通过适配器检查行为，使用合成标记消息，不让私有逐字内容进入日志／证据。确认父来源仍活跃且未变化，子会话没有可自动执行的复制队列，并且重连／重启后两份历史仍独立。发现矛盾时必须明确修订方案或保持不支持，不能放宽验收。

## 验证约定

使用已注册的 `host`、`dsh`、`core`、`cli`、`proto` 和 `access` 项目 ID。实现期间运行 `./repo check --project core`、`./repo check --project cli`，并仅在 proto/access 发生变化时运行其支持的检查。Host 和 DSH 仅支持测试操作：使用 `./repo test --project host --coverage` 和 `./repo test --project dsh --coverage`，不得对这两个 Node 项目调用 check/build。也要为发生变化的每个适用 Rust/Go 包运行覆盖率测试；每个适用包／crate 的实测覆盖率至少为 90%。预期生成变更使用 `./repo proto`，并运行 `./repo proto --check`、`./repo test --suite integration --group protocol`；这一跨组件恢复变更还需运行 `./repo test --suite integration`。原生 DSH 检查使用隔离配置和合成内容，证据只在 `.local-docs/acceptance/` 记录安全标识／计数。隔离测试通过不证明原生持久化或生产部署行为。

### Shared contract dependency

Shared capability, invocation-identity, lifecycle and input-state types are implemented once by the first consumer. A dependency on these types is not a requirement to enable another entire feature first. Respect actual protocol/source prerequisites and independent capability verification.

## Native parity review evidence

Controller fork()/latestCompletedPrefixBoundary() and dsh-session/lib/types/fork.js select the latest completed prefix including eligible standalone events, preserve source cwd, use native inherited preset/current default-model behavior, append native seed closers and create without a prompt. Arbitrary atSeq exists natively but remains excluded by explicit D-FORK-01; do not claim complete native feature parity.

Evidence is from the locally installed DSH 0.2.2-rc.1 packages, reviewed read-only. Native runtime/integration acceptance has not run. Existing transport authorization, command deduplication, encryption, privacy and uncertain-outcome recovery remain intentional remote-adapter boundaries.

## Capability acceptance gates

These are logical operation gates, not mandatory new wire flags. Follow the [evidence contract](../../README.md#acceptance-evidence-contract): all listed AC variants need current evidence, and source behavior needs the mapped native procedures. A flag covering several operations is enabled only when all its operation gates pass. Negative unsupported-capability tests passing do not enable an unverified operation. Existing shipped operations remain governed by their existing support; report any regression separately.

| Operation | Required ACs (all mandatory variants) | Required native evidence subset |
|---|---|---|
| Latest-complete-turn fork | AC-01–AC-12 | AC-01–AC-03, AC-05, AC-07, AC-10, AC-11 |

Record each operation as enabled or disabled, its gate evidence, blockers and tested native version in the delivery summary. Keep any disabled path unavailable and retain FAIL/BLOCKED/NOT RUN results. A partial rollout is not full acceptance. The [combined workflow cases](../add-dsh-session-creation/design.md#combined-workflow-acceptance) qualify the integrated feature set separately and do not block independent delivery of verified operations.
