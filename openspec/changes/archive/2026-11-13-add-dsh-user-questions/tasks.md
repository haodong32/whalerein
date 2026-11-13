# 实施任务

Before defining numeric acceptance boundaries, inspect DSH validation and the existing wire/observation constants. Record each reused limit and its source in the design; separately justify any necessary additional limit and obtain review for observable product changes. Native-runtime evidence remains required before capability enablement. For D-QUEST-01, verify exact live runtime roots, currently delegated children, and lineage-bearing sessions resumed as roots on both question paths; do not infer runtime ownership from parent_session_id.

Decision dependencies: see [Decision Review](design.md#decision-review). D-REL-01 permits independent delivery only after each enabled capability and its required dependencies are verified. Disabled or unverified capabilities remain BLOCKED or NOT RUN, never PASS; report their scope and reasons separately. Read the confirmed decisions before implementation. Establish missing native behavior and limit evidence before enabling affected capabilities; report technical blockers without inventing product restrictions.

## 1. 原生问题可行性和契约

- [x] 1.1 协调 `add-dsh-session-wait` 状态字段和投递调用上下文约定；设计合成真实 DSH 验证流程，覆盖 open 本地优先/远程优先竞争、有时限的 attachWait 认领释放/断连，以及 continued `answer()` 重复/归属行为。将脱敏结果记录在 `.local-docs/acceptance/`；对应路径通过验证前不得启用能力，路径失败时须先显式评审设计，再更改范围（AC-03 至 AC-10）。
- [x] 1.2 在 `proto/v1/whalerein.proto` 定义问题/回答/能力/请求/响应类型，重新生成绑定，扩展数据源校验和有界页/游标响应。增加协议夹具，覆盖缺失时为 false 的能力、ID、守卫、原生答案字段类型与已有协议批次预算、超限列表条目的不可用元数据、完整内容读取/超限回答明确返回 payload_too_large，以及符合完整协议响应预算的页；通过 `./repo proto`、`./repo proto --check`、`./repo test --project proto`、`./repo test --project host` 验证（AC-01、AC-02、AC-05、AC-08、AC-10、AC-11）。
- [x] 1.3 仅根据已实现的归一化类型、分阶段回执和守卫更新 WIRE/Harness 契约；使用往返编码夹具核对示例，保留当前审批契约（AC-01、AC-04、AC-05、AC-10）。

## 2. 数据源桥接和命令生命周期

- [x] 2.1 在 `host/dsh/src/adapter/` 增加职责聚焦的 DSH 问题桥接器和假数据源支持，归一化权威投影/活动句柄和根归属。实现独立且经过验证的 open 与 continued 路径，在不支持时显式委托，并提供阶段/纪元/摘要守卫，以及设备重连/重载后的新状态重建；明确测试仅设备重连时保留精确 open 句柄，与数据源/适配器重启后句柄消失的区别。在 `host/dsh/test/` 增加各生命周期及不使用兜底聊天变更的单元用例；通过 `./repo test --project dsh` 验证（AC-01 至 AC-06、AC-08 至 AC-11）。
- [x] 2.2 在 `host/runtime/src/commands/service.js` 扩展 Host 查询/控制路由，保留待处理预检前先去重、稳定调用身份、确定的调用前错误，以及调用后 unknown 结果。在 `host/runtime/test/commands.test.js` 和数据源测试中增加日志、响应丢失、摘要变更、作用域和边界用例；通过 `./repo test --project host` 验证（AC-02 至 AC-11）。
- [x] 2.3 更新 DSH 插件指南，仅记录已验证的路径能力门槛、精确的本地回答器共存方式及重启限制；依据原生夹具核对文档流程，未验证的环境限制留在本地（AC-03、AC-04、AC-06、AC-09、AC-10）。

## 3. 带类型客户端和显式人工输入

- [x] 3.1 在 `core/src/api/` 和对话类型中新增带类型的列出/回答 API 及问题状态处理，复用带守卫的命令提交/恢复和新的作用域内基线。增加错误作用域、撤销授权/离线设备、过期守卫、入队与结算区分、原生响应丢失测试；通过 `./repo test --project core` 和 `./repo check --project core` 验证（AC-01、AC-04、AC-05、AC-07 至 AC-11）。
- [x] 3.2 在 `apps/cli/src/{cli,run,output}.rs` 增加 CLI `questions` 与 `answer` 的解析、分发和人工可读/JSON 格式化；要求会话、批次、版本/摘要，以及从文件或标准输入提供的完整结构化回答。增加解析和标记脱敏测试，确认不自动回答、不默认接受，错误中不回显回答文本；通过 `./repo test --project cli` 和 `./repo check --project cli` 验证（AC-01 至 AC-05、AC-10 至 AC-12）。
- [x] 3.3 根据已实现的显式回答及 reply_queued 语义更新 CLI 用法和 Core 架构/契约；使用夹具测试核对示例，不包含私有内容（AC-03、AC-04、AC-07、AC-12）。

## 4. 跨组件验收

- [x] 4.1 在 `tests/integration/` 为全部 AC-01–AC-14 增加 CLI 到 FakeDSH 及协议用例，覆盖错误会话读取、重复回答、已撤销设备、重启和回执丢失；运行 `./repo test --suite integration --group protocol` 和 `./repo test --suite integration`。使用 `./repo test --project dsh --coverage`、`./repo test --project host --coverage`、`./repo test --project core --coverage`、`./repo test --project cli --coverage` 验证受影响包覆盖率；每个受影响包/crate 要求至少 90%，与集成结果独立判断。
- [x] 4.2 使用合成内容执行 design.md 中真实 DSH 的根 Agent、有时限、continued、本地/远程夹具流程，记录版本和脱敏证据，并依据最新检查填写每个验收结果行。失败、阻塞和未运行路径保持可见且关闭，不能将夹具通过表述为原生验收通过（AC-01–AC-14）。
- [x] 4.3 根据 design.md 评审仅适配器访问原生、共享 Host 拥有日志、Core 自包含、无明文日志及不新增基础设施等边界；记录并解决实质架构偏差，提交评审。依据最终已验证行为评估维护文档，并将合理的不更新决策记录在本地（AC-01–AC-14）。


## Native parity regression work

- [x] 5.1 In the affected protocol, adapter, Host, Core and CLI paths already listed above, implement and verify intent/detail and exact label/custom roundtrips, timed versus indefinite discovery, live root versus owned child, scoped waterfall/attachWait cooperation, and independent continued capability. Run affected ./repo unit/coverage checks, protocol checks and full integration as specified above, plus controlled native probes for source behavior; assess maintained-doc changes. Cover AC-01–AC-04, AC-06, AC-08–AC-10, AC-12, AC-13. These are unexecuted tasks, not proof of native acceptance.


## Acceptance case matrix

Follow the [acceptance evidence contract](../../README.md#acceptance-evidence-contract). Case-family IDs below are planned: expand each semicolon-separated variant into a stable subcase ID, bind it to an exact automated test selector and (where required) a native procedure before acceptance, and attach every subresult to its parent AC row. Existing feature tasks own implementation; task 6.1 owns full matrix verification. Slashes name distinct required alternatives, not permission to test only one. Tests belong in the affected paths already identified above.

| AC | Planned case family | Mandatory variants | Required verification | Owner |
|---|---|---|---|---|
| AC-01 | `user-questions/AC-01.*` | exact-live-root; lineage-session-resumed-as-root; timed-projection; available-live-provider; stable-order | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-02 | `user-questions/AC-02.*` | duplicate-ID; missing-ID; invalid-field-type; zero-settlement; native-accepted-unoffered/duplicate/multiple/custom-values-preserved | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-03 | `user-questions/AC-03.*` | valid-open-answer; native-single-settlement; no-task-completion-claim | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-04 | `user-questions/AC-04.*` | continued-accepted; reply-still-queued; later-native-admission; duplicate-queued-reply | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-05 | `user-questions/AC-05.*` | changed-version/digest/epoch; open-to-continued; expiry; unavailable-request; no-cross-phase-fallback | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-06 | `user-questions/AC-06.*` | local-first; remote-first; simultaneous; loser-no-fallback; later-request-unclaimed | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-07 | `user-questions/AC-07.*` | sequential/concurrent-repeat; changed-digest; lost-receipt; result-store-fault; restart-no-replay | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-08 | `user-questions/AC-08.*` | revoked; foreign-session/batch; currently-owned-child; no-existence-leak | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-09 | `user-questions/AC-09.*` | device-reconnect-with-live-handle; adapter-restart; source-restart; no-Promise-reconstruction | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-10 | `user-questions/AC-10.*` | old/absent-capabilities; unverified-open-disabled; independent-continued; no-local-request-interception | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-11 | `user-questions/AC-11.*` | native-schema-budget-boundaries; response/answer-budget-below/at/above; unavailable-metadata; no-partial-answer; redaction | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-12 | `user-questions/AC-12.*` | human/JSON-output; explicit-file/stdin-input; offline-refusal; no-auto-answer | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-13 | `user-questions/AC-13.*` | plan-review-detail/intent; approval-detail/intent; exact-label/custom-roundtrip; projection-missing-context; indefinite-discovery | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-14 | `user-questions/AC-14.*` | disconnect; explicit-cancel; claim-expiry; stale-late-submit; subsequent-question-not-captured; native-local-provider-availability | Automated + native procedure | 6.1 + existing AC-linked tasks |

- [x] 6.1 Implement the planned matrix subcases in the existing protocol/adapter/Host/Core/CLI test paths as applicable; record exact selectors, commands, assertions and native procedures. Run focused ./repo checks and full integration as required above, plus the mapped native procedures. Reconcile every subcase before setting a parent PASS; unresolved bounds or unavailable required native procedures remain BLOCKED. Assess capability gates and report partial delivery separately.

## 验收结果

实施任务勾选不代表验收通过。规划期间未执行实现或运行验收。

| AC 编号 | 状态 | 证据 | 限制 |
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
| AC-13 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
| AC-14 | PASS | `.data/runs/open-full-5/report.json` + change-specific native/CLI cases | Windows/macOS/other-version matrix remains unverified; see acceptance index |
