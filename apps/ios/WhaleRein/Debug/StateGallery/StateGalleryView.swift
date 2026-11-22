// Debug-only gallery of every debuggable state.
//
// It reads the same ScenarioCatalog as previews and UI tests, applies a
// scenario by rebuilding the runtime group, and opens the selected entry. It is
// the only runtime switcher, so a state cannot exist in a test without being
// inspectable here.

#if DEBUG
import SwiftUI

struct StateGalleryView: View {
    @Environment(AppRuntime.self) private var runtime
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""

    private var groups: [(category: Scenario.Category, scenarios: [Scenario])] {
        let all = ScenarioCatalog.all
        let filtered = query.isEmpty
            ? all
            : all.filter {
                $0.id.localizedCaseInsensitiveContains(query)
                    || $0.title.localizedCaseInsensitiveContains(query)
                    || $0.notes.localizedCaseInsensitiveContains(query)
            }
        return Scenario.Category.allCases.compactMap { category in
            let scenarios = filtered.filter { $0.category == category }
            guard !scenarios.isEmpty else { return nil }
            return (category, scenarios)
        }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent("Current scenario", value: runtime.environment.scenario.id)
                        .accessibilityIdentifier("gallery.current")
                }
                ForEach(groups, id: \.category) { group in
                    Section(group.category.title) {
                        ForEach(group.scenarios) { scenario in
                            Button {
                                apply(scenario)
                            } label: {
                                row(scenario)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("gallery.scenario.\(scenario.id)")
                        }
                    }
                }
            }
            .searchable(text: $query, prompt: "Search scenarios")
            .navigationTitle("State gallery")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .accessibilityIdentifier("gallery.done")
                }
            }
        }
    }

    private func row(_ scenario: Scenario) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(scenario.title)
                    .font(.body.weight(.medium))
                Spacer()
                Text(scenario.id)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            Text(scenario.notes)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text("Opens \(scenario.entry.title)")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .contentShape(Rectangle())
    }

    private func apply(_ scenario: Scenario) {
        // Rebuild first, then dismiss so the new group is the one presented.
        // The rebuilt group applies the scenario entry from its own start().
        runtime.apply(scenario)
        dismiss()
    }
}
#endif
