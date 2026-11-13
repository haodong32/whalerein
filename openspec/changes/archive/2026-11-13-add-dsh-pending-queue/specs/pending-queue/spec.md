# 规格增量


## Purpose

让远程用户能够查看并修改尚未执行的会话指令，安全地编辑、撤回或提升具体条目，同时维持来源对队列与执行顺序的权威，避免复制执行状态，也避免把排队工作误认为已经提交的对话历史。

## ADDED Requirements

### Requirement: 待执行队列读取以来源为准
系统 SHALL 返回完整的当前待执行队列，包含不透明条目身份、位置、规范化文本或附件元数据及支持的操作，不将其视为已提交历史。

#### Scenario: AC-01 完整且有界的队列读取
<a id="ac-01"></a>

- **WHEN** 已授权调用方读取一个可用队列，且其完整编码响应符合经查证的原生及现有协议预算
- **THEN** 返回带有会话身份、已观察状态和条目操作的完整有序队列；不包含原生 JSON、工具参数或原生序列位置


### Requirement: 队列可用性明确
系统 SHALL 区分可用空队列与不可读或不支持的队列，并 SHALL NOT 仅为读取而激活冷会话。

#### Scenario: AC-02 区分不可用与空队列
<a id="ac-02"></a>

- **WHEN** 实时队列为空，或者来源无法读取被动队列
- **THEN** 前者返回可用且含零项的队列；后者返回 unavailable 或 unsupported_capability，绝不宣称没有待执行工作


### Requirement: 待执行可编辑文本可以变更
来源对条目公布编辑支持时，系统 SHALL 允许对仍待执行的用户条目进行纯文本编辑。

#### Scenario: AC-03 Native whole-content text replacement
<a id="ac-03"></a>

- **WHEN** an authorized caller explicitly edits an eligible pending user item, including one with attachments, using nonblank replacement text
- **THEN** native replacement retains item identity, source and placement while replacing its complete content; the interface explains that original attachments are not retained, and no second instruction is added

### Requirement: 待执行用户工作可以撤回
系统 SHALL 允许移除仍待执行且可移除的用户条目，不取消当前生成或其他队列条目。

#### Scenario: AC-04 定向移除
<a id="ac-04"></a>

- **WHEN** 调用方移除一个待执行条目
- **THEN** 仅撤回该次待执行条目；其已提交对话历史保持不变


### Requirement: 排队工作可以进入当前轮引导
系统 SHALL 仅允许在当前轮引导仍可用时，提升符合条件的下一轮用户条目。

#### Scenario: AC-05 提升为当前轮引导
<a id="ac-05"></a>

- **WHEN** 运行中的会话提升符合条件的条目
- **THEN** 同一工作仅一次移动至引导，不产生第二个待执行副本；若能证明位置不合格或阶段不可用，则在变更前返回 conflict


### Requirement: 并发消费不恢复已执行工作
系统 SHALL 对已证明不再待执行的目标返回 not_found；并发编辑 SHALL 遵循来源执行顺序，不承诺比较并交换语义。

#### Scenario: AC-06 条目消费与并发编辑
<a id="ac-06"></a>

- **WHEN** DSH 在变更前消费条目，或者两次合法编辑竞争一个仍待执行的条目
- **THEN** 已消费条目不被重新创建；待执行条目的编辑遵循来源顺序，后续读取显示来源结果，不虚构版本号保证


### Requirement: 队列变更保留持久化命令语义
系统 SHALL 对队列变更去重，拒绝同一命令身份下的变更载荷，并在变更可能已应用后保留不确定性。

#### Scenario: AC-07 重放与不确定变更
<a id="ac-07"></a>

- **WHEN** 变更被重复提交、载荷被修改，或者原生回执或完成日志写入丢失
- **THEN** 相同命令最多调用一次原生接口；变更载荷返回 payload_digest_mismatch；回执丢失结果保持 unknown，不重放来源变更


### Requirement: 中断和移除保持独立
系统 SHALL 保留取消当前生成与撤回排队工作的区别。

#### Scenario: AC-08 中断保留待执行工作
<a id="ac-08"></a>

- **WHEN** 调用方中断一个包含待执行条目的活动会话
- **THEN** 中断回执不宣称队列已清空；新读取展示剩余工作，每次撤回分别确认


### Requirement: 队列数据采用安全会话授权
系统 SHALL 要求读取和变更具有当前有效授权，并 SHALL 将队列内容排除在日志、诊断和 Host 运行状态持久化之外。

#### Scenario: AC-09 授权与私有内容
<a id="ac-09"></a>

- **WHEN** 已撤权或未配对设备访问队列，或者合法操作被跟踪
- **THEN** 未授权请求不读取或变更 DSH；运行证据仅包含有界身份、结果和汇总数量


### Requirement: 操作要求明确且安全的支持
The system SHALL reject unsupported operations, privileged/context-item changes, non-text replacement payloads and blank replacement text. Native edit replaces the whole eligible item, not only its text fragments; original attachments do not themselves invalidate a text replacement. Keeping privileged/context entries read-only is an explicit WhaleRein authorization boundary, not a claimed DSH restriction.

#### Scenario: AC-10 能力与变更边界
<a id="ac-10"></a>

- **WHEN** 调用方尝试不支持的操作，编辑上下文条目、提交非文本替换内容，或者提供空白替换文本
- **THEN** 请求按情况以 unsupported_capability、conflict 或 invalid_argument 失败；不移除、重写或提升任何条目


### Requirement: 队列操作保持有界
The system SHALL respect verified native and existing protocol budgets, accounting for envelope overhead, reject oversized complete responses and edits explicitly, and never silently truncate. Under D-LIMITS-01, any necessary additional item/size bound requires evidence and separate review.

#### Scenario: AC-11 队列响应和编辑边界
<a id="ac-11"></a>

- **WHEN** 当前队列超过任一响应边界，或者替换文本超过既有逻辑协议限制
- **THEN** 操作返回 payload_too_large；不将部分队列作为完整队列展示，无效编辑不改变来源工作


### Requirement: 明确刷新队列状态
系统 SHALL 将队列数据作为在线观察状态，并 SHALL 在重连或变更后获取新的权威读取，才将队列展示为当前状态。

#### Scenario: AC-12 重连后的新鲜状态
<a id="ac-12"></a>

- **WHEN** 连接被替换，或者变更回执之后的刷新失败
- **THEN** 旧数据标记为过期或不可用；即使刷新失败，已确认变更仍保持确认，后续新读取替换旧视图
