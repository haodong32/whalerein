// Retains unmatched local rows while the app navigates between conversations.
// The shell replaces the store when it activates a different Core instance.

@MainActor
final class ConversationOutgoingStore {
    struct Row {
        let message: ConversationMessage
        var submission: ConversationModel.OptimisticMessage
    }

    private var detached: [SessionContext: [Row]] = [:]
    private weak var active: ConversationModel?

    func attach(_ model: ConversationModel) -> [Row] {
        active = model
        return detached.removeValue(forKey: model.outgoingScope) ?? []
    }

    func detach(_ model: ConversationModel, rows: [Row]) {
        guard active === model else { return }
        active = nil
        detached[model.outgoingScope] = rows.isEmpty ? nil : rows
    }

    func updateClosedSend(
        scope: SessionContext,
        localID: String,
        submission: ConversationModel.OptimisticMessage?,
        receipt: CommandReceipt?
    ) {
        if let active, active.outgoingScope == scope {
            if let receipt {
                active.settlePendingSend(localID: localID, receipt: receipt)
            } else {
                active.removePendingSend(localID: localID)
            }
            return
        }
        guard var rows = detached[scope],
              let index = rows.firstIndex(where: { $0.message.messageId == localID }) else { return }
        if let submission {
            rows[index].submission = submission
        } else {
            rows.remove(at: index)
        }
        detached[scope] = rows.isEmpty ? nil : rows
    }
}
