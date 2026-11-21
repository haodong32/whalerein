// Shared client-service selector for the welcome surface and Settings.
//
// Production builds offer official Cloud production and Standalone. A Debug or
// test build may also offer Staging and Local. Local still needs the desktop
// mkcert root to be trusted by iOS at runtime, so a connection may fail when it
// is not installed. Switching always requires an explicit confirmation.

import SwiftUI
import UniformTypeIdentifiers

struct ServiceSelectionView: View {
    var showsStandaloneCA = false

    @Environment(AppModel.self) private var app
    @State private var pending: AppServiceOption?
    @State private var showConfirmation = false
    @State private var isSwitching = false
    @State private var isImportingCA = false
    @State private var isImportingLocalCA = false
    @State private var errorKey: L10nKey?

    private var options: [AppServiceOption] {
        guard let configuration = app.configuration else { return [] }
        return AppConfigurationStore.serviceOptions(
            for: configuration.instances,
            showsStaging: AppConfigurationStore.defaultShowsStaging,
            showsLocal: AppConfigurationStore.defaultShowsLocal
        )
    }

    private var local: AppInstance? {
        app.configuration?.instances.first(where: { $0.isLocal })
    }

    /// Local can be created only together with its imported mkcert root.
    private func needsLocalImport(_ option: AppServiceOption) -> Bool {
        option.kind == .createLocal && !(local?.hasCloudCA ?? false)
    }

    private var standalone: AppInstance? {
        app.configuration?.instances.first(where: { $0.isStandalone })
    }

    var body: some View {
        if app.configuration != nil {
            VStack(alignment: .leading, spacing: 10) {
                Text(L10n.serviceSelectionTitle.key)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("service.selection")
                activeRow
                ForEach(options) { option in
                    optionRow(option)
                }
                if isSwitching {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text(L10n.serviceSwitching.key)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityIdentifier("service.switching")
                }
                if let errorKey {
                    Label {
                        Text(errorKey.key)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle.fill")
                    }
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("service.error")
                }
                if showsStandaloneCA, let standalone {
                    standaloneCASection(standalone)
                }
                if let local {
                    localCASection(local)
                }
            }
            .alert(
                L10n.serviceSwitchTitle.string,
                isPresented: $showConfirmation,
                presenting: pending
            ) { option in
                Button(L10n.commonCancel.key, role: .cancel) {
                    pending = nil
                }
                Button(L10n.serviceSwitchAction.key) {
                    pending = nil
                    Task { await switchTo(option) }
                }
                .accessibilityIdentifier("service.confirm")
            } message: { _ in
                Text(L10n.serviceSwitchMessage.key)
            }
            .fileImporter(
                isPresented: $isImportingCA,
                allowedContentTypes: [UTType(filenameExtension: "pem") ?? .plainText, .plainText],
                allowsMultipleSelection: false
            ) { result in
                importCA(result)
            }
            .fileImporter(
                isPresented: $isImportingLocalCA,
                allowedContentTypes: [UTType(filenameExtension: "pem") ?? .plainText, .plainText],
                allowsMultipleSelection: false
            ) { result in
                importLocalCA(result)
            }
        }
    }

    private var activeRow: some View {
        LabeledContent(L10n.serviceActive.key) {
            Text(activeLabel)
                .accessibilityIdentifier("service.active")
        }
    }

    private var activeLabel: String {
        let id = app.activeInstance?.id ?? app.configuration?.configuration.activeInstanceID ?? ""
        if let option = options.first(where: { $0.id == id }) {
            return option.titleKey.string
        }
        return id
    }

    private func optionRow(_ option: AppServiceOption) -> some View {
        let isActive = app.activeInstance?.id == option.id ||
            (option.kind == .createStaging && app.activeInstance?.environment == .staging)
        return Button {
            guard !isActive else { return }
            if needsLocalImport(option) {
                errorKey = nil
                isImportingLocalCA = true
                return
            }
            pending = option
            showConfirmation = true
        } label: {
            HStack {
                Text(option.titleKey.key)
                    .foregroundStyle(.primary)
                Spacer()
                if isActive {
                    Image(systemName: "checkmark")
                        .foregroundStyle(.tint)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isSwitching || isActive)
        .accessibilityIdentifier("service.option.\(option.id)")
        .accessibilityValue(isActive ? L10n.serviceActive.string : "")
    }

    @ViewBuilder
    private func standaloneCASection(_ instance: AppInstance) -> some View {
        Divider()
        VStack(alignment: .leading, spacing: 8) {
            Text(L10n.serviceStandaloneCA.key)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            Text(instance.hasStandaloneCA ? L10n.serviceStandaloneCAImported.string : L10n.serviceStandaloneCANone.string)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("service.standaloneCA.status")
            Button {
                errorKey = nil
                isImportingCA = true
            } label: {
                Label(L10n.serviceStandaloneCAImport.key, systemImage: "doc.badge.plus")
            }
            .disabled(isSwitching)
            .accessibilityIdentifier("service.standaloneCA.import")
            if instance.hasStandaloneCA {
                Button(role: .destructive) {
                    errorKey = nil
                    Task { await removeCA(instance) }
                } label: {
                    Label(L10n.serviceStandaloneCARemove.key, systemImage: "trash")
                }
                .disabled(isSwitching)
                .accessibilityIdentifier("service.standaloneCA.remove")
            }
        }
    }

    @ViewBuilder
    private func localCASection(_ instance: AppInstance) -> some View {
        Divider()
        VStack(alignment: .leading, spacing: 8) {
            Text(L10n.serviceLocalCA.key)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            Text(
                instance.hasCloudCA
                    ? L10n.serviceLocalCAImported.string
                    : L10n.serviceLocalCANone.string
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("service.localCA.status")
            Text(L10n.serviceLocalCANote.key)
                .font(.caption2)
                .foregroundStyle(.secondary)
            Button {
                errorKey = nil
                isImportingLocalCA = true
            } label: {
                Label(L10n.serviceLocalCAImport.key, systemImage: "doc.badge.plus")
            }
            .disabled(isSwitching)
            .accessibilityIdentifier("service.localCA.import")
            if instance.hasCloudCA {
                Button(role: .destructive) {
                    errorKey = nil
                    Task { await removeLocalCA() }
                } label: {
                    Label(L10n.serviceLocalCARemove.key, systemImage: "trash")
                }
                .disabled(isSwitching)
                .accessibilityIdentifier("service.localCA.remove")
            }
        }
    }

    private func importLocalCA(_ result: Result<[URL], Error>) {
        errorKey = nil
        switch result {
        case .failure:
            errorKey = .errorConfiguration
        case .success(let urls):
            guard let url = urls.first else {
                errorKey = .errorConfiguration
                return
            }
            let access = url.startAccessingSecurityScopedResource()
            defer {
                if access { url.stopAccessingSecurityScopedResource() }
            }
            guard let data = try? Data(contentsOf: url),
                  let pem = String(data: data, encoding: .utf8) else {
                errorKey = .errorConfiguration
                return
            }
            isSwitching = true
            Task {
                let code = await app.enableLocal(pem: pem)
                if let code {
                    errorKey = L10nKey.errorKey(for: code)
                }
                isSwitching = false
            }
        }
    }

    private func removeLocalCA() async {
        guard !isSwitching else { return }
        isSwitching = true
        let code = await app.removeLocalCA()
        if let code {
            errorKey = L10nKey.errorKey(for: code)
        }
        isSwitching = false
    }

    private func switchTo(_ option: AppServiceOption) async {
        guard !isSwitching else { return }
        isSwitching = true
        errorKey = nil
        let code = await app.switchToService(option)
        if let code {
            errorKey = L10nKey.errorKey(for: code)
        }
        isSwitching = false
    }

    private func removeCA(_ instance: AppInstance) async {
        guard !isSwitching else { return }
        isSwitching = true
        let code = await app.removeStandaloneCA()
        if let code {
            errorKey = L10nKey.errorKey(for: code)
        }
        isSwitching = false
    }

    private func importCA(_ result: Result<[URL], Error>) {
        errorKey = nil
        switch result {
        case .failure:
            errorKey = .errorConfiguration
        case .success(let urls):
            guard let url = urls.first else {
                errorKey = .errorConfiguration
                return
            }
            let access = url.startAccessingSecurityScopedResource()
            defer {
                if access { url.stopAccessingSecurityScopedResource() }
            }
            do {
                let data = try Data(contentsOf: url)
                guard let pem = String(data: data, encoding: .utf8) else {
                    errorKey = .errorConfiguration
                    return
                }
                Task {
                    let code = await app.importStandaloneCA(pem)
                    if let code {
                        errorKey = L10nKey.errorKey(for: code)
                    }
                }
            } catch {
                errorKey = .errorConfiguration
            }
        }
    }
}
