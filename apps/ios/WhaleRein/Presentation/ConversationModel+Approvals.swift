// Conversation approvals behaviour, split out of the model file.

import Foundation

@MainActor
extension ConversationModel {
    // MARK: - Approval review context

    /// Review facts the Host projected for this exact variant. Details are
    /// shown only while the call is still reviewable; terminal cards and
    /// projections the Host marked unavailable expose nothing.
    func approvalReview(_ approval: Approval) -> ApprovalReviewDetails? {
        guard let review = approval.review,
              review.availability != .unavailable else {
            return nil
        }
        switch approval.state {
        case .pending, .changed: return review
        default: return nil
        }
    }

    /// A pending approval stays allow-able even when the Host cannot describe
    /// every review fact. DSH 0.2.2-rc.1 projects only tool name, call id and
    /// reason, so requiring a complete projection would make real remote
    /// approval impossible. Allow still needs an actionable card (pending,
    /// unexpired, online, no decision in flight); the decision itself stays
    /// bound to the reviewed version/digest by `resolveApproval`, and the Host
    /// remains the authority on the device's approval right.
    func canAllowApproval(_ approval: Approval) -> Bool {
        isApprovalActionable(approval)
    }

    /// The limitation the card states alongside Allow, or `nil` when the
    /// reviewed context is complete.
    func approvalReviewLimitationKey(_ approval: Approval) -> L10nKey? {
        switch approval.state {
        case .pending, .changed:
            break
        default:
            return nil
        }
        guard let review = approval.review else { return .approvalReviewMissing }
        switch review.availability {
        case .complete: return review.isComplete ? nil : .approvalReviewIncomplete
        case .incomplete: return .approvalReviewIncomplete
        case .unavailable: return .approvalDetailsHidden
        }
    }

    // MARK: - Approvals

    func resolveApproval(_ approval: Approval, allow: Bool) async {
        guard isApprovalActionable(approval) else {
            if approval.state == .pending, let reason = approvalActionDisabledReasonKey {
                approvalErrorKey = reason
            }
            return
        }
        // Bind the decision to the exact facts on screen. A newer card, a
        // changed version/digest or changed review facts require a fresh
        // review instead of silently submitting the old intent.
        guard let current = approvals.first(where: { $0.approvalId == approval.approvalId }),
              current.version == approval.version,
              current.digestHex.caseInsensitiveCompare(approval.digestHex) == .orderedSame,
              current.review == approval.review else {
            approvalErrorKey = .approvalStale
            return
        }
        // An incomplete or unavailable projection does not block Allow: the
        // card has already stated the limitation, and the fresh version/digest
        // above is the only binding the decision needs. The Host still rejects
        // a decision from a device without the current approval right.
        let review = ApprovalReview(approval: current)
        resolvingApprovalId = approval.approvalId
        approvalErrorKey = nil
        defer { resolvingApprovalId = nil }
        do {
            let updated = try await service.resolveApproval(
                hostId: hostId,
                sessionId: sessionId,
                approvalId: review.approvalId,
                version: review.version,
                digestHex: review.digestHex,
                allow: allow
            )
            applyServerApproval(updated)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            approvalErrorKey = code == .staleApproval || code == .conflict
                ? .approvalStale
                : L10nKey.errorKey(for: code)
            await refreshApprovals()
            if code == .revoked || code == .notPaired {
                reportFatalIfNeeded(code)
            }
        }
    }

    /// Read one approval's exact scoped state from the Host and update the card.
    /// A failed or out-of-scope read never leaves a stale card actionable.
    func refreshApprovalStatus(_ approval: Approval) async {
        guard approval.sessionId == sessionId else {
            approvalStatusRefresh[approval.approvalId] = .outOfScope
            return
        }
        approvalStatusRefresh[approval.approvalId] = .refreshing
        do {
            let exact = try await service.approval(
                hostId: hostId,
                approvalId: approval.approvalId
            )
            guard exact.sessionId == sessionId else {
                approvalStatusRefresh[approval.approvalId] = .outOfScope
                return
            }
            if exact.state != .pending {
                applyServerApproval(exact)
                approvalStatusRefresh[approval.approvalId] = .resolved
            } else if exact.version != approval.version
                || exact.digestHex.caseInsensitiveCompare(approval.digestHex) != .orderedSame {
                applyServerApproval(exact)
                approvalStatusRefresh[approval.approvalId] = .changed
            } else {
                applyServerApproval(exact)
                approvalStatusRefresh[approval.approvalId] = .current
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            if code == .revoked || code == .notPaired {
                reportFatalIfNeeded(code)
            }
            markApprovalUnavailable(approval.approvalId)
            approvalStatusRefresh[approval.approvalId] = .unavailable
        }
    }

    /// Refreshes approvals from the Host. Core's approval listing returns only
    /// pending requests, so the result is merged with locally decided terminal
    /// cards rather than replacing them (APPR-04/18). A still-pending approval
    /// missing from the list is marked expired; an unavailable session marks
    /// its pending cards unavailable instead of leaving a dead clickable card
    /// (APPR-19).
    func refreshApprovals(restartTimer: Bool = true) async {
        do {
            let loaded = try await service.approvals(hostId: hostId, sessionId: sessionId)
            approvalClock = now()
            mergeApprovals(loaded, missingPendingState: .expired)
            trustPendingApprovalsReportedByServer(loaded)
            if restartTimer {
                restartApprovalTimer()
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            if code == .unavailable {
                markPendingApprovalsUnavailable()
            }
            if code == .revoked || code == .notPaired {
                reportFatalIfNeeded(code)
            }
            // Other failures keep the last known approvals; the next event or
            // reconnect refreshes them.
        }
    }

    func applyServerApproval(_ approval: Approval) {
        guard approval.sessionId == sessionId else { return }
        if let index = approvals.firstIndex(where: { $0.approvalId == approval.approvalId }) {
            if shouldAccept(approval, over: approvals[index]) {
                state.approvals[index] = approval
            }
        } else {
            state.approvals.append(approval)
        }
    }

    func mergeApprovals(
        _ incoming: [Approval],
        missingPendingState: ApprovalState
    ) {
        let scoped = incoming.filter { $0.sessionId == sessionId }
        var incomingById = Dictionary<String, Approval>()
        for approval in scoped {
            incomingById[approval.approvalId] = approval
        }

        var merged = state.approvals.filter { $0.sessionId == sessionId }
        for index in merged.indices {
            let local = merged[index]
            if let server = incomingById[local.approvalId],
               shouldAccept(server, over: local) {
                merged[index] = server
            }
        }
        let knownIds = Set(merged.map(\.approvalId))
        for approval in scoped where !knownIds.contains(approval.approvalId) {
            merged.append(approval)
        }
        // Trust only survives for requests the Host still reports pending.
        let pendingIds = Set(
            merged.filter { $0.state == .pending }.map(\.approvalId)
        )
        trustedPendingApprovalIds.formIntersection(pendingIds)
        // A pending approval the Host no longer lists was resolved or expired
        // elsewhere. Keep the card with a terminal state so it cannot be
        // clicked and the user still sees what happened.
        for index in merged.indices where merged[index].state == .pending {
            if incomingById[merged[index].approvalId] == nil {
                merged[index].state = missingPendingState
            }
        }
        state.approvals = merged
    }

    /// A decided local card must never be downgraded back to pending by an
    /// older pending-only refresh. A server terminal state with a newer
    /// version still wins; a locally unavailable or expired card may recover
    /// when the Host reports it pending again.
    func shouldAccept(_ server: Approval, over local: Approval) -> Bool {
        guard local.state.isTerminal else { return true }
        if server.state == .pending {
            switch local.state {
            case .unavailable, .expired:
                return true
            case .changed:
                return server.version > local.version
            default:
                return false
            }
        }
        return server.version >= local.version
    }

    /// The Host's approval listing is the authority for validity. A pending
    /// request it still returns is valid even if the local clock disagrees.
    func trustPendingApprovalsReportedByServer(_ loaded: [Approval]) {
        let nowMs = approvalClock.timeIntervalSince1970 * 1000
        for approval in loaded
        where approval.sessionId == sessionId
            && approval.state == .pending
            && nowMs >= Double(approval.expiresAtUnixMs) {
            trustedPendingApprovalIds.insert(approval.approvalId)
        }
    }

    func markPendingApprovalsUnavailable() {
        for index in state.approvals.indices where state.approvals[index].state == .pending {
            state.approvals[index].state = .unavailable
        }
    }

    func markApprovalUnavailable(_ approvalID: String) {
        guard let index = state.approvals.firstIndex(where: { $0.approvalId == approvalID }),
              state.approvals[index].state == .pending else { return }
        state.approvals[index].state = .unavailable
    }

    func restartWatch() async {
        guard !isReconnecting else { return }
        isReconnecting = true
        watchEnded = false
        defer { isReconnecting = false }
        await startWatchConnection()
    }

    /// Starts exactly one Core watch and refreshes the authoritative facets for
    /// the selected session. Callers own the reconnect guard and any cursor
    /// reset; Core continues a restarted watch from its own committed cursor.
    func startWatchConnection() async {
        guard !isClosed, !isSuspended else { return }
        do {
            // The draft is owned by this model and its DraftService lease.
            // Restarting the watch must not re-read and overwrite in-memory
            // text with a stale persisted value.
            errorKey = nil
            if loadState != .loaded {
                state = ConversationReduce.reduce(state, .loadStarted)
            }
            try await service.startWatch(hostId: hostId, sessionId: sessionId)
            guard !isClosed else { return }
            // A background entry may have suspended the model while the watch
            // was starting. Release it instead of leaving an observation alive
            // behind the shield; the foreground resume starts it once again.
            guard !isSuspended else {
                service.stopWatch()
                return
            }
            await refreshConversationCapabilities()
            await refreshApprovals()
            await queue.refresh()
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            errorKey = L10nKey.errorKey(for: code)
            if loadState != .loaded {
                state = ConversationReduce.reduce(state, .loadFailed(code))
            } else {
                watchEnded = true
            }
            reportFatalIfNeeded(code)
        }
    }

    func reportFatalIfNeeded(_ code: AppErrorCode) {
        guard code == .revoked || code == .notPaired else { return }
        onFatalError?(code)
    }
}
