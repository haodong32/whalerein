// Shell instances behaviour, split out of the model file.

import Foundation

@MainActor
extension AppShellModel {
    // MARK: - Instance activation

    /// Selects one configured instance. The binding prepares and commits the
    /// destination document first; the old runtime is replaced only after the
    /// new service exists. Any failure keeps the previous service and selection.
    @discardableResult
    func switchInstance(id: String, force: Bool = false) async -> AppErrorCode? {
        guard let configuration, let serviceFactory else { return .configuration }
        guard !isSwitchingInstance else { return .conflict }
        guard force || id != activeInstance?.id else { return nil }
        isSwitchingInstance = true
        defer { isSwitchingInstance = false }

        // Persist the outgoing draft before leaving its instance.
        await conversation?.suspendForBackground()
        let previousInstance = activeInstance
        let previousService = service
        let previousPreferences = preferences

        do {
            try configuration.activate(id: id)
        } catch {
            return reportInstanceError(error)
        }
        let target = configuration.activeInstance
        let nextService: any SessionService
        do {
            nextService = try serviceFactory(target)
        } catch {
            // The document committed before the runtime existed; restore the
            // previous selection rather than leaving startup and runtime split.
            if let previousInstance {
                _ = try? configuration.activate(id: previousInstance.id)
            }
            return reportInstanceError(error)
        }

        previousService.stopWatch()
        conversation?.close()
        conversation = nil
        let nextPreferences = preferencesFactory?(target) ?? previousPreferences
        replaceRuntime(
            service: nextService,
            preferences: nextPreferences,
            dataDirectory: target.dataDirectory,
            instance: target
        )
        await launchRestore()
        return nil
    }

    /// Handles one selector entry, creating the debug-only Staging instance on
    /// first use before activating it.
    @discardableResult
    func switchToService(_ option: AppServiceOption) async -> AppErrorCode? {
        guard let configuration else { return .configuration }
        switch option.kind {
        case .existing(let instanceID):
            return await switchInstance(id: instanceID)
        case .createStaging:
            let input = AppInstanceInput(
                id: "cloud-staging",
                service: .cloud,
                environment: .staging,
                dataDirectory: "instances/cloud-staging/data",
                standaloneCAPEM: nil
            )
            do {
                try configuration.save(input)
            } catch {
                return reportInstanceError(error)
            }
            return await switchInstance(id: "cloud-staging")
        case .createLocal:
            // Local needs an imported mkcert root bundle; the selector collects
            // it before calling `enableLocal`. Creating the instance here would
            // fail Core validation with a generic configuration error.
            return reportInstanceError(SessionServiceError(.configuration))
        }
    }

    /// Imports the mkcert root bundle for the local Cloud instance and selects
    /// it. An invalid bundle leaves the previous document and trust unchanged.
    @discardableResult
    func enableLocal(pem: String) async -> AppErrorCode? {
        guard let configuration else { return .configuration }
        do {
            try configuration.enableLocal(pem: pem)
        } catch {
            return reportInstanceError(error)
        }
        // Creating the instance is an explicit selection, not a refresh.
        return await switchInstance(id: "cloud-local", force: true)
    }

    @discardableResult
    func removeLocalCA() async -> AppErrorCode? {
        guard let configuration,
              let local = configuration.instances.first(where: { $0.isLocal }) else {
            return nil
        }
        // A local instance cannot open without its CA, so it is removed rather
        // than left half-configured. Leave it first because the active instance
        // cannot be deleted.
        if activeInstance?.id == local.id {
            guard let fallback = configuration.instances.first(where: { $0.isCloudProduction }) else {
                return reportInstanceError(SessionServiceError(.configuration))
            }
            if let code = await switchInstance(id: fallback.id) {
                return code
            }
        }
        do {
            try configuration.removeCloudCA(instanceID: local.id)
        } catch {
            return reportInstanceError(error)
        }
        return nil
    }

    /// Imports a validated private CA for the Standalone instance. An invalid
    /// PEM leaves the previous document and trust unchanged.
    @discardableResult
    func importStandaloneCA(_ pem: String) async -> AppErrorCode? {
        guard let configuration,
              let standalone = configuration.instances.first(where: { $0.isStandalone }) else {
            return reportInstanceError(SessionServiceError(.configuration))
        }
        do {
            try configuration.importCA(instanceID: standalone.id, pem: pem)
        } catch {
            return reportInstanceError(error)
        }
        return await reselectIfActive(id: standalone.id)
    }

    /// Rebuild the runtime for a CA change only when that instance is active.
    func reselectIfActive(id: String) async -> AppErrorCode? {
        guard activeInstance?.id == id else { return nil }
        return await switchInstance(id: id, force: true)
    }

    @discardableResult
    func removeStandaloneCA() async -> AppErrorCode? {
        guard let configuration,
              let standalone = configuration.instances.first(where: { $0.isStandalone }) else {
            return reportInstanceError(SessionServiceError(.configuration))
        }
        do {
            try configuration.removeCA(instanceID: standalone.id)
        } catch {
            return reportInstanceError(error)
        }
        return await reselectIfActive(id: standalone.id)
    }

    func replaceRuntime(
        service: any SessionService,
        preferences: PreferencesStore,
        dataDirectory: URL,
        instance: AppInstance?
    ) {
        activationGeneration &+= 1
        self.service = service
        self.preferences = preferences
        self.dataDirectory = dataDirectory
        self.activeInstance = instance
        self.isPaired = preferences.value.paired
        self.selectedHostId = preferences.value.lastHostId
        self.isRevoked = false
        self.prefersInvitationAfterDeviceReset = false
        self.isOfflineCached = false
        self.isPrivacyShieldVisible = false
        self.resumeMarker = nil
        self.rootState = .restoring
        self.didBootstrap = false
        self.isBootstrapping = false
        self.dismissSheet()
        self.dismissSheet()
        self.hostList = HostListModel(service: service, preferences: preferences)
        self.sessionList = SessionListModel(service: service, preferences: preferences)
        self.diagnostics = DiagnosticsModel(
            service: service,
            stagingDirectory: Self.diagnosticsStagingDirectory(dataDirectory)
        )
        self.pairing = PairingModel(service: service)
        self.settings = SettingsModel(
            service: service,
            preferences: preferences,
            dataDirectory: dataDirectory,
            isStaging: options.staging || instance?.environment == .staging,
            isFixtureBacked: service is FixtureSessionService,
            onDeviceReset: {}
        )
        self.sessionManagement = Self.makeManagementModel(service: service)
        let available = !(service is UnavailableSessionService)
        self.featureSet = FeatureSet.resolved(instance: instance, serviceIsAvailable: available)
        self.operationRecovery = featureSet.operationRecovery
            ? Self.makeOperationRecovery(
                service: service,
                instanceId: instance?.id ?? service.instanceId
            )
            : nil
        self.cloudSignIn = featureSet.cloudAccount
            ? makeCloudSignInModel(service: service, instance: instance)
            : nil
        self.accountSettings = featureSet.sessionManagement
            ? makeAccountSettingsModel(service: service, instance: instance)
            : nil
        installCallbacks()
    }

    static func makeOperationRecovery(
        service: any SessionService,
        instanceId: String
    ) -> OperationRecoveryModel? {
        guard let recovery = service as? any OperationRecoveryService else { return nil }
        return OperationRecoveryModel(service: recovery, instanceId: instanceId)
    }

    /// Redacted diagnostic exports go to an app-owned staging directory for the
    /// selected instance, not a process-wide temporary drop box.
    static func diagnosticsStagingDirectory(_ dataDirectory: URL) -> URL {
        dataDirectory
            .appendingPathComponent("diagnostics", isDirectory: true)
    }

    /// Fixtures replace the system sheet with a deterministic callback so UI
    /// tests can cover the app-side sign-in flow. Live builds always use the
    /// real authentication session.
    func authenticator(options: LaunchOptions) -> any CloudWebAuthenticator {
        if case .fixtures = options.serviceMode {
            if options.cloudSystemSheet {
                return WebAuthenticationSessionRunner()
            }
            return FixtureCloudAuthenticator(cancels: options.cloudCancelSheet)
        }
        return WebAuthenticationSessionRunner()
    }

    /// Builds the status/default-computer/app-sign-in model for the activation
    /// that owns `service`. Revoking the current app session fences through the
    /// same signed-out path as Cloud sign-out.
    func makeAccountSettingsModel(
        service: any SessionService,
        instance: AppInstance?
    ) -> AccountSettingsModel {
        let generation = activationGeneration
        let instanceId = instance?.id ?? service.instanceId
        let model = AccountSettingsModel(service: service)
        model.onSignedOut = { [weak self] in
            guard let self,
                  self.activationGeneration == generation,
                  (self.activeInstance?.id ?? self.service.instanceId) == instanceId else {
                return
            }
            self.cloudSignIn?.markSignedOutLocally(pending: true)
            self.handleCloudSignedOut()
        }
        return model
    }

    /// Builds the Cloud model for the activation that owns `service`. Callbacks
    /// capture both the instance ID and activation generation, so a late sheet
    /// or deep link from a replaced runtime can never mutate the new one.
    func makeCloudSignInModel(
        service: any SessionService,
        instance: AppInstance?
    ) -> CloudSignInModel? {
        // A standalone instance has no Cloud account surface, even though the
        // fixture type also implements the Cloud protocol.
        guard instance?.service == .cloud,
              let cloud = service as? any CloudAccountService else { return nil }
        let generation = activationGeneration
        let instanceId = instance?.id ?? service.instanceId
        let model = CloudSignInModel(
            service: cloud,
            fence: CloudSignInFence(
                instanceId: instanceId,
                activationGeneration: generation
            ),
            authenticator: authenticator(options: options),
            fenceIsCurrent: { [weak self] in
                guard let self else { return false }
                return self.activationGeneration == generation
                    && (self.activeInstance?.id ?? self.service.instanceId) == instanceId
            }
        )
        model.onOpenHost = { [weak self] hostId in
            guard let self, self.activationGeneration == generation else { return }
            Task { @MainActor in self.finishPairing(hostId: hostId) }
        }
        model.onSignedOut = { [weak self] in
            guard let self, self.activationGeneration == generation else { return }
            self.handleCloudSignedOut()
        }
        return model
    }

    @discardableResult
    func reportInstanceError(_ error: Error) -> AppErrorCode {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        instanceErrorKey = L10nKey.errorKey(for: code)
        return code
    }
}
