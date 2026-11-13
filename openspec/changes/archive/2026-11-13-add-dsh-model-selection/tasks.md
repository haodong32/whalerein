# 模型与推理强度切换任务

Before defining numeric acceptance boundaries, inspect DSH validation and the existing wire/observation constants. Record each reused limit and its source in the design; separately justify any necessary additional limit and obtain review for observable product changes. Native-runtime evidence remains required before capability enablement.

Decision dependencies: see [Decision Review](design.md#decision-review). D-REL-01 permits independent delivery only after each enabled capability and its required dependencies are verified. Disabled or unverified capabilities remain BLOCKED or NOT RUN, never PASS; report their scope and reasons separately. Read the confirmed decisions before implementation. Establish missing native behavior and limit evidence before enabling affected capabilities; report technical blockers without inventing product restrictions.

实现复选框不代表验收结果。保留 AC 标识，并在下表记录新证据。当前仅编写方案，下列任务均未执行。

## 1. 建立原生启用证据与协议契约

- [x] 1.1 Reconcile shared Host capabilities, then verify native selection admission, passive cold reads, default-save effects, and later local changes using a dedicated configuration. Record supported versions and limits under `.local-docs/acceptance/`; keep unverified operations disabled. Include running, queued, input-waiting, and cold cases without inventing additional state gates (AC-01, AC-03, AC-04, AC-07, AC-11, AC-14).
- [x] 1.2 在 `proto/v1/whalerein.proto` 中增加类型化模型目录/选择读取、选择命令、独立能力、影响范围和回执字段，协调七项变更的字段编号。补充缺省值、往返和字节上限夹具，按已实现公共契约更新 `proto/v1/WIRE.md`。通过 `./repo proto`、`./repo proto --check` 和 `./repo test --suite integration --group protocol` 验证（AC-01、AC-02、AC-04、AC-05、AC-13）。

## 2. 实现执行源负责的读取与修改

- [x] 2.1 扩展 `host/runtime/src/contracts/source.js` 及命令/查询分发，提供默认不支持的类型化读取和写入命令日志的会话选择，包含能力、授权及载荷校验；不新增副作用确认或状态准入限制。增加能力缺失、授权、重复、摘要变化和日志故障的执行源契约/命令测试。使用 `./repo test --project host --coverage` 验证；该 Node 包没有注册 check 脚本（AC-01、AC-05、AC-08、AC-09、AC-10）。
- [x] 2.2 在 `host/dsh/src/adapter/{base,real,fake}.js` 实现模型目录和冷会话安全的选择归一化：隔离提供方失败，复用经查证的原生／既有协议响应预算；额外并发限制须提供依据并单列说明，被动读取不激活原生 Agent。使用不透明哨兵值增加原生上下文调用监视、部分目录与精确大小边界测试。使用 `./repo test --project dsh --coverage` 验证；该 Node 包没有注册 check 脚本（AC-02、AC-03、AC-13）。
- [x] 2.3 Implement selection through native `selectModel`, preserving native admission and default-save behavior. Remove the proposed acknowledgement flag and idle/empty-queue/no-input gates. Test native accepted/rejected/uncertain outcomes, cold restoration, later local changes, and background save failures with `./repo test --project dsh --coverage` and task 1.1 native checks (AC-04–AC-07, AC-10, AC-11, AC-14).
- [x] 2.4 仅将实现和证据支持的原生行为与启用限制更新到 `docs/architecture/dsh-plugin.md` 和 `host/dsh/README.md`；逐条对照任务 2.1–2.3，剩余原生发布限制保留在本地记录（AC-03、AC-04、AC-07、AC-14）。

## 3. 增加类型化 Core 和 CLI 操作

- [x] 3.1 在 `core/src/api/` 增加目录/读取/选择门面 API，复用现有 Host 身份、查询期限及持久化命令恢复；拒绝离线写入，保留原回执，不新增持久化模型缓存。增加门面/恢复测试，通过 `./repo check --project core` 和 `./repo test --project core --coverage` 验证（AC-01、AC-02、AC-03、AC-08、AC-09、AC-10、AC-11、AC-12）。
- [x] 3.2 Add `models`, `model get`, and `model set` parsing/dispatch/output in `apps/cli/src/`. Display native effect scope without requiring a separate acknowledgement argument. Test human/JSON receipts, valid selection without extra confirmation, native rejections, unknown outcomes, and unconfirmed persistence. Update verified usage in `apps/cli/README.md`; run `./repo check --project cli`, `./repo test --project cli --coverage`, and `./repo build --project cli` (AC-01, AC-02, AC-04–AC-07, AC-10, AC-12, AC-14).

## 4. 跨组件验证与评审

- [x] 4.1 增加 CLI 到 FakeDSH 的模型目录/选择集成覆盖，以及故障驱动重连、重复请求、撤销和离线处理，不记录提供方秘密或原生模型调用载荷。通过 `./repo test --suite integration` 验证；任务 1.2 后若绑定再次变更，则重新执行协议定向检查（AC-01 至 AC-14 中可由夹具验证的部分；保留原生限制）。
- [x] 4.2 完成专用配置下真实 DSH 被动读取、选择/默认值副作用、后续本地更改和本地并发启动流程。在 `.local-docs/acceptance/` 记录命令/步骤、准确版本、仅含元数据的观测与限制。确认材料区分原生准入与客户端额外限制、后台持久化与会话成功（AC-03、AC-04、AC-06、AC-07、AC-11、AC-14）。
- [x] 4.3 对照新证据和受影响包至少 90% 的实测覆盖率，逐行核对验收结果；明确标出失败、阻塞和未执行检查。不得仅凭 OpenSpec 校验或源码阅读宣称运行时验收通过（AC-01 至 AC-14）。
- [x] 4.4 对照设计评审最终适配器/Core/Access 边界和配置范围，记录偏差；核对前述公共契约/用法文档与最终实现一致，或说明不需进一步更新维护文档的原因。带日期证据与任务历史保留本地（AC-01 至 AC-14）。


## Acceptance case matrix

Follow the [acceptance evidence contract](../../README.md#acceptance-evidence-contract). Case-family IDs below are planned: expand each semicolon-separated variant into a stable subcase ID, bind it to an exact automated test selector and (where required) a native procedure before acceptance, and attach every subresult to its parent AC row. Existing feature tasks own implementation; task 6.1 owns full matrix verification. Slashes name distinct required alternatives, not permission to test only one. Tests belong in the affected paths already identified above.

| AC | Planned case family | Mandatory variants | Required verification | Owner |
|---|---|---|---|---|
| AC-01 | `model-selection/AC-01.*` | absent-capabilities; catalog-only; disabled-mutation | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-02 | `model-selection/AC-02.*` | healthy-catalog; one-provider-fails; stable-sanitized-failure | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-03 | `model-selection/AC-03.*` | cold-no-selection; pending/last-used/default-distinction; no-activation | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-04 | `model-selection/AC-04.*` | effort-omitted; effort-selected; receipt-effect-scope; no-prompt | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-05 | `model-selection/AC-05.*` | no-acknowledgement-flag; no-confirmation-conflict | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-06 | `model-selection/AC-06.*` | stale-model-route; invalid-effort; native-proven-prechange-rejection | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-07 | `model-selection/AC-07.*` | running; waiting-input; queue-nonempty; queue-unknown; cold-restoration | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-08 | `model-selection/AC-08.*` | revoked-catalog; revoked-read; revoked-selection; zero-native-calls | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-09 | `model-selection/AC-09.*` | sequential-repeat; concurrent-repeat; changed-digest | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-10 | `model-selection/AC-10.*` | lost-receipt; encoding-fault; result-store-fault; reconnect/restart-no-replay | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-11 | `model-selection/AC-11.*` | local-change-after-receipt; fresh-read; unchanged-recovered-receipt | Automated + native procedure | 6.1 + existing AC-linked tasks |
| AC-12 | `model-selection/AC-12.*` | offline-catalog; offline-read; offline-selection; no-deferred-write | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-13 | `model-selection/AC-13.*` | encoded-budget-below/at/above; sensitive-provider-error-redaction | Automated fixtures/integration | 6.1 + existing AC-linked tasks |
| AC-14 | `model-selection/AC-14.*` | accepted-selection-background-save-success/failure; no-retry-or-rollback | Automated + native procedure | 6.1 + existing AC-linked tasks |

- [x] 6.1 Implement the planned matrix subcases in the existing protocol/adapter/Host/Core/CLI test paths as applicable; record exact selectors, commands, assertions and native procedures. Run focused ./repo checks and full integration as required above, plus the mapped native procedures. Reconcile every subcase before setting a parent PASS; unresolved bounds or unavailable required native procedures remain BLOCKED. Assess capability gates and report partial delivery separately.

## 验收结果

| 验收编号 | 状态 | 证据 | 限制 |
| --- | --- | --- | --- |
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
