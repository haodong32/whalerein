# 规格增量


## Purpose

让已授权远程用户发现并显式回答结构化人工问题，同时保留原生请求归属、生命周期与结算语义，并通过明确的阶段、守卫及能力边界防止错误回答和重复提交。

## ADDED Requirements

### Requirement: 作用域内待处理问题发现
系统 SHALL 提供原生待处理问题批次，包含不透明批次身份、问题 ID、题干、原生 detail/header、选项标签与描述、multiSelect、原生 intent、open/continued 状态以及版本/摘要守卫；回答保留 selected 标签及 optional custom，不新增没有原生依据的自由文本开关。

<a id="scenario-ac-01-list-native-question-batches"></a>

<a id="ac-01"></a>
#### Scenario: AC-01 列出原生问题批次
- **WHEN** an authorized user lists questions for a natively supported session, including a lineage-bearing session resumed as the exact live runtime root
- **THEN** 响应仅按数据源顺序包含该会话当前待处理批次，不含提供方重放载荷；审批请求保持独立类型

### Requirement: 完整回答校验
Answers SHALL preserve native selected labels and optional custom text. Each batch SHALL name every question ID exactly once and use the protocol field types. SHALL NOT add unverified option, selection-count or free-text restrictions, or infer permission grants from option position.

<a id="scenario-ac-02-malformed-or-incomplete-answer"></a>

<a id="ac-02"></a>
#### Scenario: AC-02 格式错误或不完整的回答
- **WHEN** 回答重复或遗漏问题 ID、字段类型无效，或包含 DSH 接受的未知/重复/多个选项标签或自定义文本
- **THEN** 无效 ID 或字段类型返回 invalid_argument 且不结算；对其余原生接受的值按原样交给 DSH，不添加未验证的业务限制

### Requirement: open 问题结算
对于受支持且仍活动的 open 批次，系统 SHALL 通过原生待处理请求交付经过校验的显式人工回答，并返回已确认的提交回执。

<a id="scenario-ac-03-answer-an-open-question"></a>

<a id="ac-03"></a>
#### Scenario: AC-03 回答 open 问题
- **WHEN** 匹配的 open 批次仍活动，用户提交有效且符合当前守卫的回答
- **THEN** 一个原生请求使用所提交回答完成结算，响应标识该批次；客户端不声称后续 Agent 工作已经完成

### Requirement: continued 问题回复
对于受支持的 continued 批次，系统 SHALL 使用原生 continued 回答路径，并区分已接收回复的准入与问题最终结算。

<a id="scenario-ac-04-answer-after-the-foreground-window"></a>

<a id="ac-04"></a>
#### Scenario: AC-04 前台窗口结束后回答
- **WHEN** continued 批次仍可回答，且收到有效匹配的回答提交
- **THEN** 回执返回 reply_queued；仅在权威数据源状态确认准入后，批次才结算；不伪造重复普通聊天消息

### Requirement: 版本和状态守卫
回答 SHALL 在原生调用时校验批次摘要、版本、纪元和可回答性，且 SHALL NOT 将 open 回答静默转到 continued 路径。

<a id="scenario-ac-05-stale-display-or-changed-phase"></a>

<a id="ac-05"></a>
#### Scenario: AC-05 显示过期或阶段变化
- **WHEN** 列出问题后，问题发生变更、从 open 进入 continued、过期或变为不可用
- **THEN** 在能够证明结果时，回答在原生变更前返回 conflict 或 question_unavailable；用户必须刷新并针对新阶段显式重新提交

### Requirement: 本地与远程竞争
并发本地/远程回答 SHALL 最多产生一次原生结算或 continued 回复准入；失败的提交 SHALL 返回 conflict 或 unavailable。

<a id="scenario-ac-06-local-and-remote-answer-simultaneously"></a>

<a id="ac-06"></a>
#### Scenario: AC-06 本地和远程同时回答
- **WHEN** 本地回答器与远程控制同时尝试回答同一批次
- **THEN** 仅有一个回答被原生权威接纳；失败方不发送兜底消息、不覆盖胜出回答，也不抢占未来本地请求

### Requirement: 命令去重和不确定性
回答控制 SHALL 保留稳定命令身份和摘要去重；未确认的原生副作用 SHALL 保持 unknown，且 SHALL NOT 自动重放。

<a id="scenario-ac-07-duplicate-command-or-lost-receipt"></a>

<a id="ac-07"></a>
#### Scenario: AC-07 重复命令或回执丢失
- **WHEN** 回答命令重复、以修改后的载荷复用，或者原生调用后回执丢失
- **THEN** 相同摘要的重复请求恢复已存储结果，不二次调用；不同摘要复用被拒绝；不确定副作用保持 unknown，由显式结果恢复处理

### Requirement: 会话授权
问题读取和回答 SHALL 要求当前授权与精确的主机/会话/批次作用域，并 SHALL 拒绝不支持的委托原生归属。

<a id="scenario-ac-08-wrong-session-or-revoked-device"></a>

<a id="ac-08"></a>
#### Scenario: AC-08 错误会话或已撤销设备
- **WHEN** 设备授权被撤销、批次属于其他会话，或者原生调用方是被其他 Agent 持有的子 Agent，而非受支持的活动根 Agent
- **THEN** 不暴露问题内容或原生回答；返回稳定的授权、未找到或不支持结果，不跨作用域泄露批次存在性

### Requirement: 重连和原生生命周期
重连或原生重启后，当前问题 SHALL 根据权威状态和活动句柄重建；不可用的 open 请求 SHALL NOT 根据缓存内容重新创建。

<a id="scenario-ac-09-reconnect-or-restart-during-a-question"></a>

<a id="ac-09"></a>
#### Scenario: AC-09 问题期间重连或重启
- **WHEN** 设备重连时其精确原生 open 请求仍活动，或者数据源/适配器重启后原有 open 句柄已消失
- **THEN** 仅设备重连时，在重新校验作用域、阶段、版本与活动句柄后，可重新暴露 open 批次；数据源/适配器重启绝不根据缓存文本重建已消失的 open Promise，仅暴露通过受支持原生路径独立确认仍可回答的批次

### Requirement: 能力兼容性
问题读取、open 回答和 continued 回答 SHALL 分别公告能力标记，缺失时默认为 false。

<a id="scenario-ac-10-unverified-or-older-source"></a>

<a id="ac-10"></a>
#### Scenario: AC-10 未验证或旧版数据源
- **WHEN** 数据源未提供标记，或对应路径的原生仲裁尚未验证
- **THEN** 对应操作返回 unsupported_capability，不拦截原生问题；旧审批行为保持不变

### Requirement: 内容和资源边界
Question and answer content SHALL remain encrypted in transit and SHALL NOT enter diagnostics, plaintext Host runtime state, or plaintext Access storage. Counts and sizes SHALL follow verified native/protocol constraints under D-LIMITS-01, not the earlier draft product limits. Any additional bound requires documented evidence and review.

<a id="scenario-ac-11-private-or-oversized-question-batch"></a>

<a id="ac-11"></a>
#### Scenario: AC-11 私有或超限的问题批次
- **WHEN** a question batch or answer violates verified native validation or the existing protocol budget, including encoded envelope overhead
- **THEN** 列表对超限批次仅返回有界的不可用元数据；完整内容读取和超限回答返回 payload_too_large，不部分接纳回答；正常及错误诊断只含允许的元数据，不含问题或回答文本

### Requirement: 显式 CLI 回答输入
CLI SHALL 列出归一化问题，并从文件或标准输入接受带版本/摘要守卫的显式结构化回答；SHALL NOT 自动回答或离线回答。

<a id="scenario-ac-12-human-and-json-cli-flow"></a>

<a id="ac-12"></a>
#### Scenario: AC-12 人工可读与 JSON CLI 流程
- **WHEN** 用户列出批次、提交带守卫的结构化输入，或尝试依据离线缓存状态回答
- **THEN** 人工可读/JSON 输出区分 open 结算与 reply_queued；非法输入错误可供用户处理；拒绝离线提交，不将其输入当作新聊天消息

### Requirement: Preserve native question context

The system SHALL preserve available native detail/header, option labels/descriptions and typed intent, along with selected/custom answer encoding. Missing live context SHALL NOT be fabricated from a timed projection or transcript.

<a id="ac-13"></a>
#### Scenario: AC-13 Preserve native question context

- **WHEN** a live question carries plan-review/approval detail, or only a timed projection/legacy provider observation is available
- **THEN** show the complete available decision context and return exact selected labels/custom text; distinguish projection-only discoverability from active answerability, never infer approval by position, and leave unverified open-provider arbitration disabled

### Requirement: Release open-question claims

Remote disconnect, cancellation or native claim expiry SHALL release the matching open-answer claim according to the native lifecycle without manufacturing an answer.

<a id="ac-14"></a>
#### Scenario: AC-14 Release open-question claims

- **WHEN** a remote open-answer client disconnects, cancels, or loses its claim before answering, including a late submission after release
- **THEN** release or expire the exact claim; preserve native local/provider handling of still-active or subsequent questions, reject stale submissions without a second settlement, and prevent the old handle from claiming later requests. Keep remote open capability disabled if this cannot be verified
