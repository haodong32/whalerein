# 实施任务

Before defining numeric acceptance boundaries, inspect DSH validation and the existing wire/observation constants. Record each reused limit and its source in the design; separately justify any necessary additional limit and obtain review for observable product changes. Native-runtime evidence remains required before capability enablement.

Decision dependencies: see [Decision Review](design.md#decision-review). D-REL-01 permits independent delivery only after each enabled capability and its required dependencies are verified. Disabled or unverified capabilities remain BLOCKED or NOT RUN, never PASS; report their scope and reasons separately. Read the confirmed decisions before implementation. Establish missing native behavior and limit evidence before enabling affected capabilities; report technical blockers without inventing product restrictions.

实施任务完成不代表验收成功。保留无关的既有改动，开始实施前评审架构和所有验收评审行。

## 1. 协议与主机生命周期契约

- [x] 1.1 协调当前协议／恢复改动，在 `proto/v1/whalerein.proto` 增加 `host_capabilities`、HostCapabilities 创建支持，以及带类型创建／生命周期回执负载；通过 `./repo proto` 重新生成，增加跨语言夹具，验证 `./repo proto --check` 和协议夹具单元测试（AC-01–AC-03、AC-05、AC-07、AC-12）。
- [x] 1.2 扩展 `host/runtime/src/contracts/source.js`，提供与来源无关的 `describeHostCapabilities()` 和 `executeHost(command)` 默认行为；扩展 `commands/service.js`，不绕过既有日志；增加 `source-contract.test.js`／`commands.test.js` 场景，证明旧控制不变、不支持时的默认行为，以及不使用虚拟会话目标；验证 `./repo test --project host`（AC-04、AC-05、AC-08–AC-10）。
- [x] 1.3 增加生命周期回执校验／编码，确保明文 Host 结果日志不含 cwd/workspace/preset 内容；独立获取实际摘要，不改变已确认结果；在变更前校验原生／协议长度以及可省略但不能同时出现的 cwd/workspace 语义；在变更后注入编码／日志失败，通过 `./repo test --project host` 验证 unknown 且不重放；仅按已实现行为更新长期维护的 Harness 契约和 WIRE 文档（AC-03、AC-07–AC-10、AC-12）。

## 2. DSH 来源适配

- [x] 2.1 增加真实适配器服务能力检测、主机平台绝对路径校验，以及原生 create 调用：保留可省略位置与可选 sessionId，遵循默认目录、空白复用和返回实际身份语义；扩展模拟适配器和真实上下文测试，覆盖目录／工作区／默认／预设／不支持／目标缺失；验证 `./repo test --project dsh`（AC-01–AC-03、AC-05、AC-06、AC-10）。
- [x] 2.2 将安全的结构化工作区绑定失败回执归一化为已确认创建且带警告；普通不确定错误保持非确定性；增加夹具调用计数和脱敏断言，验证 `./repo test --project dsh`（AC-06–AC-09、AC-12）。
- [x] 2.3 确认已创建身份可用于既有 list/get/history 投影，刷新失败不重新提交创建；增加适配器和 Host 测试框架场景，验证针对性的 `./repo test --project dsh` 和 `./repo test --project host`；仅在最终边界有实质差异时更新 DSH 架构（AC-07、AC-10、AC-11）。

## 3. Core 与 CLI 控制路径

- [x] 3.1 在 `core/src/api/types.rs` 增加归一化创建输入／结果／能力类型，在 `core/src/api/core.rs` 沿用既有持久化不确定意图恢复进行主机目标提交；增加离线、授权撤销／错误 Host、回执丢失／重启和旧 Host 测试；验证 `./repo test --project core`（AC-01–AC-05、AC-07–AC-09、AC-11、AC-12）。
- [x] 3.2 在 `apps/cli/src/{cli,run,output}.rs` 增加 CLI `session-create` 解析／派发及人类可读／JSON 处置状态输出；使用合成夹具输入测试选择项冲突与限制、接纳和确认的区别、部分警告、unknown 恢复身份及私有诊断脱敏；验证 `./repo test --project cli`（AC-01–AC-03、AC-05、AC-07、AC-09、AC-12）。
- [x] 3.3 增加长期维护的 CLI 使用说明，涵盖独立创建与发送、原生目录语义、工作区／预设标识来源和不支持的主机；对照夹具驱动的可执行行为验证示例，将带日期的证据留在本地（AC-01–AC-03、AC-05、AC-11、AC-12）。

## 4. 集成与原生验收

- [x] 4.1 在 `tests/integration` 增加协议／全链路场景，覆盖目录／工作区、已知原生失败、授权撤销／过期命令、重复／摘要改变的并发、部分结果和崩溃恢复；先验证 `./repo test --suite integration --group protocol`，再验证 `./repo test --suite integration`（AC-01–AC-14）。
- [x] 4.2 通过适配器运行隔离的真实 DSH 0.2.2-rc.1 检查：新身份、明确主机目录、工作区／预设、创建后绑定失败封装、列表／读取／独立首条消息，以及重启后的持久化；仅在 `.local-docs/acceptance/` 保存安全证据，契约冲突时保持能力不可用（AC-01、AC-02、AC-06、AC-07、AC-10、AC-11）。
- [x] 4.3 对发生变化的 core/cli/proto/access 项目执行支持的静态检查，对每个变化的 host/dsh/core/cli/access 包运行 `./repo test --project ID --coverage`；host/dsh 仅有测试能力，MUST NOT 对其调用 check/build；验证各包实际覆盖率达到 90%，并运行 `./repo proto --check`；缺失测量或工具必须记录为失败／限制，不能算通过（AC-01–AC-14）。
- [x] 4.4 使用新鲜命令／检查证据和限制核对每条验收结果；评估架构／依赖偏差及长期维护文档需求，确认文档只包含已实现的持久行为；完成前报告未解决的范围冲突（AC-01–AC-14）。


## Native parity regression work

- [x] 5.1 In the affected protocol, adapter, Host, Core and CLI paths already listed above, implement and verify omitted default directory, optional preferred-ID blank reuse, named-new/fallback-new behavior, changed preset, workspace warning after reuse, native recursive directory creation, and independent unnamed commands. Run affected ./repo unit/coverage checks, protocol checks and full integration as specified above, plus controlled native probes for source behavior; assess maintained-doc changes. Cover AC-01–AC-03, AC-07, AC-10, AC-13, AC-14. These are unexecuted tasks, not proof of native acceptance.


## Acceptance case matrix

Follow the [acceptance evidence contract](../../README.md#acceptance-evidence-contract). Case-family IDs below are planned: expand each semicolon-separated variant into a stable subcase ID, bind it to an exact automated test selector and (where required) a native procedure before acceptance, and attach every subresult to its parent AC row. Existing feature tasks own implementation; task 6.1 owns full matrix verification. Slashes name distinct required alternatives, not permission to test only one. Tests belong in the affected paths already identified above.

| AC | Planned case family | Mandatory variants | Required verification | Owner |
|---|---|---|---|---|
| AC-01 | `session-creation/AC-01.*` | new-explicit-directory; native-directory-creation; no-prompt; private-receipt-fields-absent | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-02 | `session-creation/AC-02.*` | workspace-with-preset; workspace-default-preset; actual-binding-observed | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-03 | `session-creation/AC-03.*` | both-location-fields; relative-path; verified-length-below/at/above | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-04 | `session-creation/AC-04.*` | unpaired; revoked; wrong-Host; expired-before-admission | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-05 | `session-creation/AC-05.*` | old-Host; capability-absent; capability-false; zero-native-mutation | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-06 | `session-creation/AC-06.*` | workspace-missing; preset-missing; proven-native-prechange-denial | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-07 | `session-creation/AC-07.*` | workspace-attach-after-new; workspace-attach-after-reuse; retained-identity-no-retry | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-08 | `session-creation/AC-08.*` | sequential-repeat; concurrent-repeat; changed-digest | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-09 | `session-creation/AC-09.*` | lost-response; result-store-fault; Core-restart; Host-restart; recovered-same-identity | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-10 | `session-creation/AC-10.*` | different-devices-no-preferred-ID; different-commands-same-directory | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-11 | `session-creation/AC-11.*` | list/get/history-same-identity; separate-first-message; refresh-fails-no-recreate | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-12 | `session-creation/AC-12.*` | offline-no-deferred-create; human/JSON-dispositions; synthetic-private-field-redaction | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-13 | `session-creation/AC-13.*` | omitted-location; Host-default-not-client-directory; default-preset | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-14 | `session-creation/AC-14.*` | blank-reuse; started-fallback; archived-fallback; missing-ID-native-create; ownership-conflict; cwd-conflict; preset-change; attach-warning-after-reuse; concurrent-same-preferred-ID | Automated + native procedure | 6.1 + existing AC-linked tasks |

- [x] 6.1 Implement the planned matrix subcases in the existing protocol/adapter/Host/Core/CLI test paths as applicable; record exact selectors, commands, assertions and native procedures. Run focused ./repo checks and full integration as required above, plus the mapped native procedures. Reconcile every subcase before setting a parent PASS; unresolved bounds or unavailable required native procedures remain BLOCKED. Assess capability gates and report partial delivery separately.

## 验收结果

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

## Combined workflow verification

- [x] 7.1 Implement planned integration case families `dsh-workflows/GX-01.*` through `dsh-workflows/GX-05.*` under `tests/integration/`, using the WHEN/THEN definitions in [Combined workflow acceptance](design.md#combined-workflow-acceptance). Include default/reuse, open/continued, every enabled mutation's response-loss path, parent/child isolation and partial-rollout variants. Bind exact test selectors and native procedures before running. Execute protocol checks, `./repo test --suite integration` and the isolated native remote procedures; record individual subresults and safe evidence locally. This qualifies the combined feature set and does not block independently verified feature delivery.
- [x] 7.2 Reconcile GX subresults, every feature's capability gates, outstanding source/environment blockers, architectural deviations and maintained-docs decisions. Report partial feature delivery and combined-workflow acceptance separately; retain unverified/failed cases on archive rather than declaring the whole set complete.

## Combined workflow results

| Group case | Status | Evidence | Limitations |
|---|---|---|---|
| GX-01 | PASS | `open-full-5` group cases | Windows/macOS/other-version matrix unverified |
| GX-02 | PASS | `open-full-5` group cases | Windows/macOS/other-version matrix unverified |
| GX-03 | PASS | `open-full-5` group cases | Windows/macOS/other-version matrix unverified |
| GX-04 | PASS | `open-full-5` group cases | Windows/macOS/other-version matrix unverified |
| GX-05 | PASS | `open-full-5` group cases | Windows/macOS/other-version matrix unverified |
