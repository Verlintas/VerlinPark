import SwiftUI

/// S8 team-template library. Everything here is a bounded read or an explicit
/// operator action: preview never writes, apply requires the approval toggle,
/// and the daemon creates only pending S6 hire requests.
struct TeamTemplatesCard: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 12) {
            header
            if let error = model.teamTemplateError {
                ProjectOrgErrorBanner(error: error) {
                    Task { await model.loadTeamTemplates() }
                }
            }
            if model.selectedProjectID == nil {
                ProjectOrgEmptyState(
                    icon: "person.3.sequence.fill",
                    title: "Select a project",
                    detail: "A team charter is always validated and applied against one target project."
                )
            } else if model.teamTemplateEntries.isEmpty {
                if model.teamTemplatesBusy {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Reading template library…")
                            .font(VPTheme.mono(8))
                            .foregroundStyle(VPTheme.textSecondary)
                    }
                } else {
                    ProjectOrgEmptyState(
                        icon: "doc.badge.plus",
                        title: "No team templates",
                        detail: "Export a charter with `vp team-export` or paste team.json to preview an import."
                    )
                }
            } else {
                VStack(spacing: 6) {
                    ForEach(model.teamTemplateEntries) { entry in
                        TeamTemplateRow(entry: entry)
                    }
                }
            }
            if let notice = model.teamTemplateNotice {
                Text(L10n.t(notice))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.accent)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let report = model.teamTemplateReplay {
                Text(
                    "audit \(report.events) events · \(report.templates) templates · \(report.links) links · \(report.repairedLinks) repaired"
                )
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textTertiary)
            }
        }
        .padding(16)
        .vpSurface(radius: 16)
        .vpHoverLift(scale: 1.002, lift: 1)
        .vpAppear(index: 1)
        .sheet(item: $model.teamTemplateSheet) { sheet in
            TeamTemplateSheetView(sheet: sheet)
                .environment(model)
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                SectionLabel(title: "Team templates")
                Text("Portable charters")
                    .font(.system(size: 14, weight: .semibold, design: .rounded))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(
                    L10n.t(
                        model.teamTemplatesStatus
                            ?? "Structure and rules only; no memory, inbox, worktree, or performance evidence."
                    )
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(2)
            }
            Spacer()
            if model.teamTemplatesBusy {
                ProgressView().controlSize(.small)
            }
            Button {
                Task { await model.replayTeamTemplates(repair: false) }
            } label: {
                Label("Audit", systemImage: "checkmark.seal")
                    .font(VPTheme.mono(8))
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.selectedProjectID == nil || model.teamTemplatesBusy)
            .help("Compare the project template audit stream with stored links. Read-only.")
            Button {
                model.presentTeamTemplateImport()
            } label: {
                Label("Import", systemImage: "square.and.arrow.down")
                    .font(VPTheme.mono(8))
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.selectedProjectID == nil)
            .help("Paste team.json, preview the org chart, then explicitly approve an apply.")
            Button {
                Task { await model.loadTeamTemplates() }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
                    .font(VPTheme.mono(8))
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.selectedProjectID == nil || model.teamTemplatesBusy)
        }
    }
}

private struct TeamTemplateRow: View {
    @Environment(AppModel.self) private var model
    let entry: TeamTemplateEntry

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: entry.applied ? "checkmark.seal.fill" : "doc.text")
                .font(.system(size: 11))
                .foregroundStyle(entry.applied ? VPTheme.success : VPTheme.textTertiary)
                .padding(.top, 1)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(entry.name)
                        .font(VPTheme.font(10.5, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                        .lineLimit(1)
                    Text("v\(entry.version)")
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                    TeamTemplateBadge(
                        text: entry.applied ? "applied" : (entry.readOnly ? "read-only" : "library"),
                        color: entry.applied ? VPTheme.success : VPTheme.textTertiary
                    )
                }
                Text("\(entry.templateID) · \(shortHash(entry.contentHash))")
                    .font(VPTheme.mono(7))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(1)
                if let source = entry.sourceProject {
                    Text("source \(source)")
                        .font(VPTheme.mono(7))
                        .foregroundStyle(VPTheme.textTertiary)
                }
            }
            Spacer(minLength: 4)
            Button {
                Task { await model.loadTeamTemplateDiff(templateID: entry.templateID) }
            } label: {
                Label("Diff", systemImage: "arrow.left.arrow.right")
                    .font(VPTheme.mono(7.5))
            }
            .buttonStyle(.bordered)
            .controlSize(.mini)
            .help("Diff this charter against the target project's current roster. Read-only.")
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 8)
        .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 9))
    }

    private func shortHash(_ hash: String) -> String {
        hash.count > 12 ? String(hash.prefix(12)) + "…" : hash
    }
}

private struct TeamTemplateBadge: View {
    let text: String
    let color: Color

    var body: some View {
        Text(L10n.t(text).uppercased())
            .font(VPTheme.mono(6.8, weight: .semibold))
            .tracking(0.6)
            .foregroundStyle(color)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(color.opacity(0.08), in: Capsule())
            .overlay {
                Capsule().strokeBorder(color.opacity(0.18), lineWidth: 0.6)
            }
    }
}

struct TeamTemplateSheetView: View {
    @Environment(AppModel.self) private var model
    let sheet: TeamTemplateSheet

    var body: some View {
        switch sheet {
        case .importTemplate:
            TeamTemplateImportSheet()
                .environment(model)
        case .diff:
            TeamTemplateDiffSheet()
                .environment(model)
        }
    }
}

private struct TeamTemplateImportSheet: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model

        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text("IMPORT TEAM CHARTER")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.1)
                    .foregroundStyle(VPTheme.accent)
                Text("Portable structure, not context")
                    .font(VPTheme.font(19, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(
                    "Preview validates schema, hash, model references, tool names, budgets, and leakage. Apply requires explicit approval and creates only pending hire requests."
                )
                .font(VPTheme.font(10))
                .foregroundStyle(VPTheme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
            }

            if let error = model.teamTemplateError {
                ProjectOrgErrorBanner(error: error, retry: nil)
            }

            TextEditor(text: $model.teamTemplateImportJSON)
                .font(VPTheme.mono(9))
                .scrollContentBackground(.hidden)
                .padding(8)
                .frame(minHeight: 190)
                .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 10))
                .overlay {
                    RoundedRectangle(cornerRadius: 10)
                        .strokeBorder(VPTheme.hairline, lineWidth: 1)
                }
                .accessibilityLabel("team.json content")

            HStack(spacing: 8) {
                Button {
                    Task { await model.previewTeamTemplateImport() }
                } label: {
                    Label("Preview", systemImage: "eye")
                }
                .buttonStyle(.bordered)
                .disabled(model.teamTemplatesBusy)

                Toggle(
                    "I approve applying this template",
                    isOn: $model.teamTemplateApplyApproved
                )
                .toggleStyle(.checkbox)
                .font(VPTheme.mono(8.5))
                .disabled(model.teamTemplatePreview?.applyAllowed != true)

                Spacer()

                Button {
                    Task { await model.applyTeamTemplateImport() }
                } label: {
                    Label("Apply", systemImage: "checkmark.seal")
                }
                .buttonStyle(.borderedProminent)
                .disabled(
                    model.teamTemplatesBusy
                        || !model.teamTemplateApplyApproved
                        || model.teamTemplatePreview?.applyAllowed != true
                )
                .help("Apply is explicit; the daemon still revalidates every gate.")
            }

            if let preview = model.teamTemplatePreview {
                TeamTemplatePreviewView(preview: preview)
            }

            if let notice = model.teamTemplateNotice {
                Text(L10n.t(notice))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.accent)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Close") { model.dismissTeamTemplateSheet() }
                    .buttonStyle(.bordered)
            }
        }
        .padding(18)
        .frame(minWidth: 760, minHeight: 640)
    }
}

private struct TeamTemplatePreviewView: View {
    @Environment(AppModel.self) private var model
    let preview: TeamTemplatePreview

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                TeamTemplateBadge(
                    text: preview.applyAllowed ? "ready" : "blocked",
                    color: preview.applyAllowed ? VPTheme.success : VPTheme.warning
                )
                Text("\(preview.name) v\(preview.version)")
                    .font(VPTheme.font(11, weight: .semibold))
                    .foregroundStyle(VPTheme.textPrimary)
                Text(String(preview.contentHash.prefix(12)) + "…")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                Spacer()
                if preview.leaderOverlay {
                    TeamTemplateBadge(text: "leader overlay", color: VPTheme.accent)
                }
            }
            if preview.leaderOverlay {
                Text("The existing project Leader is kept; the charter never overwrites it.")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textSecondary)
            } else if !preview.departments.isEmpty {
                Text("No live Leader exists; Manager/Worker slots are blocked until a Leader is created.")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.warning)
            }

            if !preview.proposedOrg.isEmpty {
                VStack(alignment: .leading, spacing: 1) {
                    Text("PROPOSED ORG")
                        .font(VPTheme.mono(7, weight: .semibold))
                        .tracking(0.8)
                        .foregroundStyle(VPTheme.textTertiary)
                    ForEach(preview.proposedOrg, id: \.self) { line in
                        Text(line)
                            .font(VPTheme.mono(7.5))
                            .foregroundStyle(VPTheme.textSecondary)
                            .lineLimit(2)
                    }
                }
            }

            ForEach(preview.departments) { department in
                VStack(alignment: .leading, spacing: 2) {
                    Text(department.name.uppercased())
                        .font(VPTheme.mono(7, weight: .semibold))
                        .tracking(0.8)
                        .foregroundStyle(VPTheme.textTertiary)
                    TeamTemplateSlotLine(slot: department.manager)
                    ForEach(department.workers) { worker in
                        TeamTemplateSlotLine(slot: worker)
                    }
                }
            }

            if !preview.conflicts.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    Text("CONFLICTS")
                        .font(VPTheme.mono(7, weight: .semibold))
                        .tracking(0.8)
                        .foregroundStyle(VPTheme.danger)
                    ForEach(preview.conflicts) { conflict in
                        HStack(alignment: .top, spacing: 6) {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("\(conflict.slot) · \(conflict.proposedTitle)")
                                    .font(VPTheme.mono(7.5))
                                    .foregroundStyle(VPTheme.textPrimary)
                                Text(L10n.t(conflict.message))
                                    .font(VPTheme.mono(7))
                                    .foregroundStyle(VPTheme.textSecondary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            Spacer(minLength: 4)
                            Button("Skip") {
                                model.addTeamTemplateResolution(action: "skip", slot: conflict.slot)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.mini)
                            if let identityID = conflict.existingIdentityID {
                                Button("Merge existing") {
                                    model.addTeamTemplateResolution(
                                        action: "merge",
                                        slot: conflict.slot,
                                        identityID: identityID
                                    )
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.mini)
                                .help("Bind the slot to this live identity without overwriting it.")
                            }
                        }
                    }
                    if !model.teamTemplateResolutions.isEmpty {
                        Button("Clear staged resolutions") {
                            model.clearTeamTemplateResolutions()
                        }
                        .buttonStyle(.borderless)
                        .font(VPTheme.mono(7.5))
                    }
                }
            }

            TeamTemplateStringList(title: "Budget notes", values: preview.budgetChanges)
            TeamTemplateStringList(title: "Model gaps", values: preview.modelGaps, danger: true)
            TeamTemplateStringList(title: "Tool gaps", values: preview.toolGaps, danger: true)
            TeamTemplateStringList(
                title: "Policy conflicts (target wins)",
                values: preview.policyConflicts,
                danger: true
            )
            TeamTemplateStringList(title: "Warnings", values: preview.warnings)
        }
        .padding(10)
        .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct TeamTemplateSlotLine: View {
    let slot: TeamTemplateSlotPreview

    var body: some View {
        HStack(spacing: 5) {
            Text(slot.role.uppercased())
                .font(VPTheme.mono(6.5, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
                .frame(width: 46, alignment: .leading)
            Text(slot.title)
                .font(VPTheme.mono(7.5))
                .foregroundStyle(VPTheme.textSecondary)
                .lineLimit(1)
            if let component = slot.component {
                Text(component)
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            Spacer()
            TeamTemplateBadge(
                text: slot.actionLabel,
                color: slot.action == "conflict"
                    ? VPTheme.danger
                    : (slot.action == "blocked" ? VPTheme.warning : VPTheme.textTertiary)
            )
        }
    }
}

private struct TeamTemplateStringList: View {
    let title: String
    let values: [String]
    var danger = false

    var body: some View {
        if !values.isEmpty {
            VStack(alignment: .leading, spacing: 1) {
                Text(title.uppercased())
                    .font(VPTheme.mono(7, weight: .semibold))
                    .tracking(0.8)
                    .foregroundStyle(danger ? VPTheme.danger : VPTheme.textTertiary)
                ForEach(values, id: \.self) { value in
                    Text("· \(L10n.t(value))")
                        .font(VPTheme.mono(7.2))
                        .foregroundStyle(VPTheme.textSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}

private struct TeamTemplateDiffSheet: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text("TEAM TEMPLATE DIFF")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.1)
                    .foregroundStyle(VPTheme.accent)
                Text(model.teamTemplateSelectedID ?? "Template")
                    .font(VPTheme.font(19, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                Text("Read-only: no identity, hire, link, or policy change.")
                    .font(VPTheme.font(10))
                    .foregroundStyle(VPTheme.textSecondary)
            }
            if let error = model.teamTemplateError {
                ProjectOrgErrorBanner(error: error) {
                    if let id = model.teamTemplateSelectedID {
                        Task { await model.loadTeamTemplateDiff(templateID: id) }
                    }
                }
            }
            if model.teamTemplatesBusy {
                ProgressView().controlSize(.small)
            }
            if let diff = model.teamTemplateDiff {
                Text("hash \(diff.contentHash.prefix(16))…")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                if diff.rows.isEmpty {
                    Text("No differences.")
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(diff.rows) { row in
                            HStack(alignment: .top, spacing: 6) {
                                TeamTemplateBadge(text: row.kind, color: VPTheme.textTertiary)
                                    .frame(width: 102, alignment: .leading)
                                Text(row.slot)
                                    .font(VPTheme.mono(7.2))
                                    .foregroundStyle(VPTheme.textPrimary)
                                    .frame(width: 150, alignment: .leading)
                                Text(L10n.t(row.detail))
                                    .font(VPTheme.mono(7.2))
                                    .foregroundStyle(VPTheme.textSecondary)
                                    .fixedSize(horizontal: false, vertical: true)
                                Spacer(minLength: 0)
                            }
                        }
                    }
                }
                .frame(minHeight: 260)
                if !diff.warnings.isEmpty {
                    TeamTemplateStringList(title: "Warnings", values: diff.warnings)
                }
            }
            HStack {
                Button {
                    if let id = model.teamTemplateSelectedID {
                        Task { await model.loadTeamTemplateDiff(templateID: id) }
                    }
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
                .buttonStyle(.bordered)
                Spacer()
                Button("Close") { model.dismissTeamTemplateSheet() }
                    .buttonStyle(.bordered)
            }
        }
        .padding(18)
        .frame(minWidth: 760, minHeight: 560)
    }
}
