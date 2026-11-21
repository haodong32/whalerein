// Settings retained-operation list. Manual query and recovery stay explicit;
// automatic foreground refresh never appears here as a mutation.

import SwiftUI

struct SettingsOperationsView: View {
    @Environment(AppShellModel.self) private var app

    var body: some View {
        Section(L10n.settingsOperations.key) {
            if let recovery = app.operationRecovery {
                if recovery.operations.isEmpty {
                    Text(L10n.settingsOperationsEmpty.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("settings.operationsEmpty")
                } else {
                    ForEach(recovery.operations) { operation in
                        row(operation, recovery: recovery)
                    }
                }
                Button {
                    Task { await recovery.load() }
                } label: {
                    Label(L10n.settingsOperationsRefresh.key, systemImage: "arrow.clockwise")
                }
                .disabled(recovery.isLoading)
                .accessibilityIdentifier("settings.operationsRefresh")

                if recovery.isLoading {
                    ProgressView()
                }
                if let errorKey = recovery.errorKey {
                    Text(errorKey.key)
                        .font(.footnote)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("settings.operationsError")
                }
            } else {
                Text(L10n.settingsOperationsUnavailable.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.operationsUnavailable")
            }
        }
        .task {
            await app.operationRecovery?.load()
        }
    }

    private func row(
        _ operation: OperationReceipt,
        recovery: OperationRecoveryModel
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 8) {
                Text(verbatim: operation.operationId)
                    .font(.caption.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 4)
                Text(verbatim: operation.status.rawValue)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 6) {
                Text(verbatim: operation.kind.rawValue)
                if let scope = operation.scope {
                    Text(verbatim: "\(scope.hostId)/\(scope.sessionId)")
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)

            if let readError = operation.readError {
                Text(L10n.settingsOperationsReadFailed.key)
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("settings.operation.readError.\(operation.operationId)")
                Text(verbatim: readError)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
            }
            if let content = operation.content {
                Text(verbatim: String(content.plainText.prefix(160)))
                    .font(.caption)
                    .lineLimit(2)
            }

            HStack(spacing: 16) {
                Button(L10n.settingsOperationsRecover.key) {
                    Task { await recovery.recover(operationId: operation.operationId) }
                }
                .font(.caption)
                .disabled(recovery.recoveringOperationId != nil)
                .accessibilityIdentifier("settings.operation.recover.\(operation.operationId)")

                if operation.canReadExactReply {
                    Button(L10n.settingsOperationsRead.key) {
                        Task { await recovery.readExactReply(operationId: operation.operationId) }
                    }
                    .font(.caption)
                    .disabled(recovery.readingOperationId != nil)
                    .accessibilityIdentifier("settings.operation.read.\(operation.operationId)")
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("settings.operation.\(operation.operationId)")
    }
}
