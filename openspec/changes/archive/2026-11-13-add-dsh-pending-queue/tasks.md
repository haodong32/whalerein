# 任务

Before defining numeric acceptance boundaries, inspect DSH validation and the existing wire/observation constants. Record each reused limit and its source in the design; separately justify any necessary additional limit and obtain review for observable product changes. Native-runtime evidence remains required before capability enablement.

Decision dependencies: see [Decision Review](design.md#decision-review). D-REL-01 permits independent delivery only after each enabled capability and its required dependencies are verified. Disabled or unverified capabilities remain BLOCKED or NOT RUN, never PASS; report their scope and reasons separately. Read the confirmed decisions before implementation. Establish missing native behavior and limit evidence before enabling affected capabilities; report technical blockers without inventing product restrictions.

## 1. 共享契约与原生可行性

- [x] 1.1 协调并复用投递方案的稳定调用身份和 delivery_id 约定。执行设计中的一次性原生流程，覆盖 queue=null 的被动读取、队列条目来源、内容规范化及 edit/remove/steer 竞争；产出仅含元数据的逐项安全操作证据。公布相关操作前验证 AC-01、AC-02、AC-05、AC-06、AC-08、AC-10。无法支持的来源保持只读；重大矛盾要求方案复核。
- [x] 1.2 在 `proto/v1/whalerein.proto` 与 Host/Rust 契约中增加来源无关的队列读取及结果、变更载荷、队列能力标志和条目操作及位置类型。不暴露原生 seq 或原始 JSON；增加跨语言测试夹具及 基于经查证原生及现有协议预算的边界夹具，不沿用未经证实的条目数／大小上限。运行 `./repo proto` 和 `./repo proto --check`；覆盖 AC-01、AC-02、AC-10、AC-11、AC-12。仅针对已实现的公共契约更新 WIRE.md。

## 2. 适配器与共享 Host

- [x] 2.1 在 DSH 适配器和 FakeDSH 中实现被动、有界队列读取，采用 `host/dsh/src/adapter/projection.js` 白名单内容规则，并明确处理 null 与空队列。扩展真实适配器和模拟测试，覆盖会话范围、条目 ID 及关联、只读内容、大小和数量失败；运行 `./repo test --project dsh` 验证。覆盖 AC-01、AC-02、AC-09、AC-10、AC-11。
- [x] 2.2 在 `host/runtime/src/commands/service.js` 中增加队列查询路由，以及写入命令日志的条目变更分发，使用逐项原生校验、稳定调用上下文和安全错误映射。增加 host-command 与适配器测试，覆盖编辑、移除、提升、并发消费、顺序编辑，以及移除后引导的不确定性；运行 `./repo test --project host` 和 `./repo test --project dsh` 验证。覆盖 AC-03 至 AC-07、AC-10。在本组任务中更新 host/dsh/README.md 中已验证的适配器能力。
- [x] 2.3 扩展 Host 故障恢复与授权测试，覆盖重放、摘要变更、原生或终态存储回执丢失、重启、已撤权设备访问和中断保留队列。检查合成操作证据是否排除了私有内容，并验证既有命令恢复仍不会重放原生调用。运行同一组 Host/DSH 单元测试；覆盖 AC-07、AC-08、AC-09、AC-12。

## 3. Core 与 CLI 队列控制

- [x] 3.1 在 `core/src/api/core.rs` 和规范化 API 类型中增加类型化在线队列查询、变更及回执恢复。队列正文不进入持久化缓存，区分过期和不可用数据，将变更确认与刷新错误分开。增加 Core 模拟传输测试，运行 `./repo test --project core`；覆盖 AC-01 至 AC-12。
- [x] 3.2 在 `apps/cli/src/{cli,run,output}.rs` 中增加 `queue list|edit|remove|steer` 解析、分发和人类可读及 JSON 输出。要求会话和条目身份，展示条目操作与可用性，变更结果不回显输入，且在变更或重连后显式刷新。增加参数、边界及回执与刷新区别的 CLI 测试；运行 `./repo test --project cli`。覆盖 AC-01 至 AC-06、AC-08 至 AC-12。在本组任务中更新已验证行为的 CLI 用法。

## 4. 集成、验收与架构审查

- [x] 4.1 增加协议与 CLI 到 FakeDSH 的集成测试，覆盖完整队列读取、每种变更、过期条目竞争、不支持或冷队列、撤权、超大结果及不确定状态恢复。运行 `./repo proto --check`、`./repo check --project core --project cli --project proto`、`./repo test --project core --project cli --project proto --project host --project dsh --coverage`、`./repo test --suite integration --group protocol`，然后运行完整的 `./repo test --suite integration`。每个受影响 Rust/Node 包要求至少 90% 单元行覆盖率。覆盖 AC-01 至 AC-12；明确保留失败和跳过结果。
- [x] 4.2 使用合成内容，通过实际安全远程路径重新执行原生队列流程，包括冷读取、本地与远程并发编辑或消费、中断保留队列及重连。以 `.local-docs/acceptance/` 中最新证据核对每行验收结果；FakeDSH 和静态检查不属于原生发布验证。
- [x] 4.3 根据 design.md 审查仅由适配器访问原生 API、DSH 拥有队列、不增加队列持久化、不虚构原子性及兼容性；报告架构偏差并评估长期维护文档需求。仅在已实现公共契约或用法变化时更新持久文档，否则记录无需更新的原因。运行 `openspec validate add-dsh-pending-queue --strict` 验证最终变更。


## Native parity regression work

- [x] 5.1 In the affected protocol, adapter, Host, Core and CLI paths already listed above, implement and verify null versus empty, explicit text replacement of original attachments, context authorization boundary, native queue-item promotion requiring running/next-turn (unlike direct prompt steer), and interrupt retaining inbox. Run affected ./repo unit/coverage checks, protocol checks and full integration as specified above, plus controlled native probes for source behavior; assess maintained-doc changes. Cover AC-02, AC-03, AC-05, AC-08, AC-10. These are unexecuted tasks, not proof of native acceptance.


## Acceptance case matrix

Follow the [acceptance evidence contract](../../README.md#acceptance-evidence-contract). Case-family IDs below are planned: expand each semicolon-separated variant into a stable subcase ID, bind it to an exact automated test selector and (where required) a native procedure before acceptance, and attach every subresult to its parent AC row. Existing feature tasks own implementation; task 6.1 owns full matrix verification. Slashes name distinct required alternatives, not permission to test only one. Tests belong in the affected paths already identified above.

| AC | Planned case family | Mandatory variants | Required verification | Owner |
|---|---|---|---|---|
| AC-01 | `pending-queue/AC-01.*` | live-complete-ordered-queue; IDs/placements/correlation; no-native-JSON | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-02 | `pending-queue/AC-02.*` | live-empty-array; cold-null; unsupported-read; no-cold-activation | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-03 | `pending-queue/AC-03.*` | plain-text-item; attachment-item-whole-replacement; retained-ID/source/placement | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-04 | `pending-queue/AC-04.*` | target-only-removal; unrelated-item-unchanged; committed-history-unchanged | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-05 | `pending-queue/AC-05.*` | eligible-running-next-turn; idle-rejection; next-step-rejection; removal-then-steer-fault | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-06 | `pending-queue/AC-06.*` | consumed-before-edit/remove; concurrent-edits; no-recreation-or-CAS-claim | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-07 | `pending-queue/AC-07.*` | sequential/concurrent-repeat; changed-digest; lost-receipt; result-store-fault; restart | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-08 | `pending-queue/AC-08.*` | interrupt-retains-inbox; separate-explicit-withdrawal; fresh-source-observation | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-09 | `pending-queue/AC-09.*` | unpaired; revoked; wrong-session-scope; synthetic-private-content-redaction | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-10 | `pending-queue/AC-10.*` | unsupported-operation; privileged/context-entry; non-text-replacement; blank-text | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-11 | `pending-queue/AC-11.*` | complete-response-budget-below/at/above; edit-budget-below/at/above; no-truncation | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-12 | `pending-queue/AC-12.*` | reconnect-stale-data; postmutation-refresh-failure; later-refresh-keeps-confirmed-result | Automated + native procedure | 6.1 + existing AC-linked tasks |

- [x] 6.1 Implement the planned matrix subcases in the existing protocol/adapter/Host/Core/CLI test paths as applicable; record exact selectors, commands, assertions and native procedures. Run focused ./repo checks and full integration as required above, plus the mapped native procedures. Reconcile every subcase before setting a parent PASS; unresolved bounds or unavailable required native procedures remain BLOCKED. Assess capability gates and report partial delivery separately.

## 验收结果

实现完成与验收独立。允许的结果：PASS、FAIL、BLOCKED、NOT RUN。

| AC编号 | 状态 | 证据 | 限制 |
|---|---|---|---|
| AC-01 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-02 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-03 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-04 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-05 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-06 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-07 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-08 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-09 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-10 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-11 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-12 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
