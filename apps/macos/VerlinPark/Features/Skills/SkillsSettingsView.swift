import SwiftUI

struct SkillsSettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var editingSkill: ProjectSkill?
    @State private var showingEditor = false
    @State private var showingImport = false
    @State private var pendingDelete: ProjectSkill?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                if model.selectedProjectID == nil {
                    SettingsEmptyState(
                        icon: "book.closed",
                        title: L10n.t("Select a project"),
                        detail: L10n.t("Skills are project-scoped instruction packs.")
                    )
                } else if model.skills.isEmpty, !model.skillsBusy {
                    SettingsEmptyState(
                        icon: "wand.and.stars",
                        title: L10n.t("No skills yet"),
                        detail: L10n.t(
                            "Create reusable review, research, or delivery instructions. Agents see the catalog and activate a skill by name."
                        )
                    )
                } else {
                    ForEach(model.skills) { skill in
                        skillCard(skill)
                    }
                }
                if let status = model.skillsStatus {
                    Text(status)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                if model.selectedSessionID != nil {
                    activationSection
                }
                Text(
                    L10n.t(
                        "Skills are instructions plus an optional tool allow/deny policy. They narrow the role's tools; they never grant new permissions. Use `*` for all tools; deny wins. The active skill replaces the previous one for the current agent run."
                    )
                )
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .padding(.top, 4)
            }
            .padding(22)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: model.selectedProjectID) {
            if model.selectedProjectID != nil {
                await model.loadSkills()
            }
        }
        .task(id: model.selectedSessionID) {
            await model.loadSkillActivations()
        }
        .sheet(isPresented: $showingEditor) {
            SkillEditorSheet(skill: editingSkill)
                .environment(model)
        }
        .sheet(isPresented: $showingImport) {
            SkillImportSheet()
                .environment(model)
        }
        .confirmationDialog(
            L10n.t("Delete this skill?"),
            isPresented: Binding(
                get: { pendingDelete != nil },
                set: {
                    if !$0 {
                        pendingDelete = nil
                    }
                }
            )
        ) {
            Button(L10n.t("Delete"), role: .destructive) {
                if let skill = pendingDelete {
                    Task { await model.deleteSkill(skill) }
                }
                pendingDelete = nil
            }
            Button(L10n.t("Cancel"), role: .cancel) { pendingDelete = nil }
        } message: {
            Text(pendingDelete?.name ?? "")
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                Text(L10n.t("Skills"))
                    .font(VPTheme.font(20, weight: .semibold))
                Text(
                    L10n.t(
                        "Reusable project instructions that agents can activate with `use_skill`."
                    )
                )
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textSecondary)
            }
            Spacer()
            if model.skillsBusy {
                ProgressView().controlSize(.small)
            }
            Button(L10n.t("Import SKILL.md")) {
                showingImport = true
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            Button(L10n.t("New skill")) {
                editingSkill = nil
                showingEditor = true
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            .disabled(model.selectedProjectID == nil)
        }
    }

    private var activationSection: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(spacing: 6) {
                Image(systemName: "bolt.badge.clock")
                    .font(.system(size: 9))
                    .foregroundStyle(VPTheme.accent)
                Text(L10n.t("Recent activations"))
                    .font(VPTheme.mono(8, weight: .semibold))
                    .foregroundStyle(VPTheme.textSecondary)
                if model.skillActivationsBusy {
                    ProgressView().controlSize(.mini)
                }
                Spacer()
                Text(L10n.t("Current session"))
                    .font(VPTheme.mono(6.8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
            if model.skillActivations.isEmpty {
                Text(L10n.t("No skill has been activated in this session yet."))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                    .padding(.vertical, 4)
            } else {
                ForEach(Array(model.skillActivations.suffix(8).reversed())) { activation in
                    HStack(alignment: .top, spacing: 7) {
                        Image(systemName: "wand.and.stars")
                            .font(.system(size: 8))
                            .foregroundStyle(VPTheme.accent)
                            .frame(width: 12)
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 5) {
                                Text(activation.name)
                                    .font(VPTheme.mono(8.5, weight: .medium))
                                Text("v\(activation.version)")
                                    .font(VPTheme.mono(6.5))
                                    .foregroundStyle(VPTheme.textTertiary)
                                if !activation.role.isEmpty {
                                    Text(activation.role.uppercased())
                                        .font(VPTheme.mono(6.2, weight: .semibold))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                                Spacer()
                                Text(activation.activatedAt)
                                    .font(VPTheme.mono(6.2))
                                    .foregroundStyle(VPTheme.textTertiary)
                                    .lineLimit(1)
                            }
                            if !activation.toolAllow.isEmpty || !activation.toolDeny.isEmpty {
                                Text(
                                    "allow: \(activation.toolAllow.isEmpty ? "role default" : activation.toolAllow.joined(separator: ", ")) · deny: \(activation.toolDeny.isEmpty ? "none" : activation.toolDeny.joined(separator: ", "))"
                                )
                                .font(VPTheme.mono(6.8))
                                .foregroundStyle(VPTheme.textTertiary)
                                .lineLimit(2)
                            }
                        }
                    }
                    .padding(7)
                    .background(VPTheme.surface.opacity(0.35), in: RoundedRectangle(cornerRadius: 8))
                }
            }
        }
        .padding(.top, 4)
    }

    private func skillCard(_ skill: ProjectSkill) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(skill.name)
                            .font(VPTheme.font(13, weight: .semibold))
                            .foregroundStyle(skill.enabled ? VPTheme.textPrimary : VPTheme.textTertiary)
                        Text("v\(skill.version)")
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                        Text(skill.source.uppercased())
                            .font(VPTheme.mono(6.2, weight: .semibold))
                            .foregroundStyle(VPTheme.accent)
                            .padding(.horizontal, 4)
                            .padding(.vertical, 1)
                            .background(VPTheme.accent.opacity(0.10), in: Capsule())
                    }
                    Text(skill.description)
                        .font(VPTheme.mono(8.5))
                        .foregroundStyle(VPTheme.textSecondary)
                        .lineLimit(2)
                }
                Spacer()
                if model.skillActionBusyID == skill.id {
                    ProgressView().controlSize(.mini)
                }
                Toggle("", isOn: Binding(
                    get: { skill.enabled },
                    set: { value in
                        Task { await model.setSkillEnabled(skill, enabled: value) }
                    }
                ))
                .toggleStyle(.switch)
                .controlSize(.mini)
                .labelsHidden()
                Button {
                    editingSkill = skill
                    showingEditor = true
                } label: {
                    Image(systemName: "pencil")
                        .font(.system(size: 9))
                }
                .buttonStyle(.plain)
                .foregroundStyle(VPTheme.textSecondary)
                .help(L10n.t("Edit skill"))
                VPCloseButton(size: 22, help: L10n.t("Delete skill")) {
                    pendingDelete = skill
                }
            }

            if !skill.whenToUse.isEmpty {
                Text("use when: \(skill.whenToUse)")
                    .font(VPTheme.mono(7.5))
                    .foregroundStyle(VPTheme.textTertiary)
                    .lineLimit(2)
            }

            HStack(spacing: 5) {
                skillChip("roles", skill.roles.isEmpty ? "all" : skill.roles.joined(separator: ", "))
                skillChip(
                    "profiles",
                    skill.profiles.isEmpty ? "all" : skill.profiles.joined(separator: ", ")
                )
                if !skill.toolAllow.isEmpty {
                    skillChip("allow", skill.toolAllow.joined(separator: ", "))
                }
                if !skill.toolDeny.isEmpty {
                    skillChip("deny", skill.toolDeny.joined(separator: ", "))
                }
                Spacer()
            }

            Text(skill.instructions)
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)
                .lineLimit(3)
                .textSelection(.enabled)
        }
        .padding(12)
        .background(
            VPTheme.surface.opacity(skill.enabled ? 0.45 : 0.22),
            in: RoundedRectangle(cornerRadius: 12)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
    }

    private func skillChip(_ label: String, _ value: String) -> some View {
        Text("\(label): \(value)")
            .font(VPTheme.mono(6.8))
            .foregroundStyle(VPTheme.textTertiary)
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(VPTheme.surface, in: Capsule())
    }
}

private struct SkillEditorSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let skill: ProjectSkill?

    @State private var name: String
    @State private var description: String
    @State private var whenToUse: String
    @State private var version: String
    @State private var instructions: String
    @State private var toolAllow: String
    @State private var toolDeny: String
    @State private var profiles: String
    @State private var roles: String
    @State private var busy = false
    @State private var error: String?

    init(skill: ProjectSkill?) {
        self.skill = skill
        _name = State(initialValue: skill?.name ?? "")
        _description = State(initialValue: skill?.description ?? "")
        _whenToUse = State(initialValue: skill?.whenToUse ?? "")
        _version = State(initialValue: skill?.version ?? "1.0.0")
        _instructions = State(initialValue: skill?.instructions ?? "")
        _toolAllow = State(initialValue: skill?.toolAllow.joined(separator: ", ") ?? "")
        _toolDeny = State(initialValue: skill?.toolDeny.joined(separator: ", ") ?? "")
        _profiles = State(initialValue: skill?.profiles.joined(separator: ", ") ?? "")
        _roles = State(initialValue: skill?.roles.joined(separator: ", ") ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(skill == nil ? L10n.t("New skill") : L10n.t("Edit skill"))
                        .font(VPTheme.font(16, weight: .semibold))
                    Text(L10n.t("Instructions and an optional tool policy."))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                VPCloseButton(size: 24, help: L10n.t("Close")) { dismiss() }
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    TextField(L10n.t("Name"), text: $name)
                        .textFieldStyle(.roundedBorder)
                    TextField(L10n.t("Short description"), text: $description)
                        .textFieldStyle(.roundedBorder)
                    TextField(L10n.t("When should an agent use it?"), text: $whenToUse)
                        .textFieldStyle(.roundedBorder)
                    HStack(spacing: 8) {
                        TextField(L10n.t("Version"), text: $version)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 100)
                        TextField(L10n.t("Roles (comma separated)"), text: $roles)
                            .textFieldStyle(.roundedBorder)
                        TextField(L10n.t("Profiles (comma separated)"), text: $profiles)
                            .textFieldStyle(.roundedBorder)
                    }
                    HStack(spacing: 8) {
                        TextField(L10n.t("Allow tools (comma separated)"), text: $toolAllow)
                            .textFieldStyle(.roundedBorder)
                        TextField(L10n.t("Deny tools (comma separated)"), text: $toolDeny)
                            .textFieldStyle(.roundedBorder)
                    }
                    Text(L10n.t("Instructions"))
                        .font(VPTheme.mono(8, weight: .semibold))
                        .foregroundStyle(VPTheme.textSecondary)
                    TextEditor(text: $instructions)
                        .font(VPTheme.mono(9.5))
                        .scrollContentBackground(.hidden)
                        .frame(minHeight: 190)
                        .padding(6)
                        .background(VPTheme.surfaceSunken, in: RoundedRectangle(cornerRadius: 8))
                        .overlay {
                            RoundedRectangle(cornerRadius: 8)
                                .strokeBorder(VPTheme.hairline, lineWidth: 1)
                        }
                }
            }

            if let error {
                Text(error)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
            } else if let status = model.skillsStatus {
                Text(status)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
            }

            HStack {
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.bordered)
                Button(skill == nil ? L10n.t("Create") : L10n.t("Save")) {
                    Task { await save() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)
            }
        }
        .padding(20)
        .frame(width: 680, height: 660)
        .background { VPCrystalBackdrop() }
    }

    private func save() async {
        busy = true
        defer { busy = false }
        let allow = parseSkillList(toolAllow)
        let deny = parseSkillList(toolDeny)
        let parsedProfiles = parseSkillList(profiles)
        let parsedRoles = parseSkillList(roles)
        let result: ProjectSkill? = if let skill {
            await model.updateSkill(
                id: skill.id,
                name: name,
                description: description,
                whenToUse: whenToUse,
                version: version,
                instructions: instructions,
                toolAllow: allow,
                toolDeny: deny,
                profiles: parsedProfiles,
                roles: parsedRoles
            )
        } else {
            await model.createSkill(
                name: name,
                description: description,
                whenToUse: whenToUse,
                version: version,
                instructions: instructions,
                toolAllow: allow,
                toolDeny: deny,
                profiles: parsedProfiles,
                roles: parsedRoles
            )
        }
        if result != nil {
            dismiss()
        } else {
            error = model.skillsStatus
        }
    }

    private func parseSkillList(_ value: String) -> [String] {
        var result: [String] = []
        for item in value.split(whereSeparator: { $0 == "," || $0 == "\n" }) {
            let trimmed = item.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty, !result.contains(trimmed) {
                result.append(trimmed)
            }
        }
        return result
    }
}

private struct SkillImportSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var path = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.t("Import SKILL.md"))
                        .font(VPTheme.font(16, weight: .semibold))
                    Text(L10n.t("Read a Markdown skill from the project root."))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                VPCloseButton(size: 24, help: L10n.t("Close")) { dismiss() }
            }
            TextField(L10n.t("Path, for example skills/review/SKILL.md"), text: $path)
                .textFieldStyle(.roundedBorder)
                .font(VPTheme.mono(9.5))
            if let error {
                Text(error)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.warning)
            } else if let status = model.skillsStatus {
                Text(status)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textSecondary)
            }
            HStack {
                Spacer()
                Button(L10n.t("Cancel")) { dismiss() }
                    .buttonStyle(.bordered)
                Button(L10n.t("Import")) {
                    Task {
                        busy = true
                        let skill = await model.importSkill(path: path)
                        busy = false
                        if skill != nil {
                            dismiss()
                        } else {
                            error = model.skillsStatus
                        }
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)
            }
        }
        .padding(20)
        .frame(width: 520)
        .background { VPCrystalBackdrop() }
    }
}

private struct SettingsEmptyState: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        VStack(spacing: 9) {
            Image(systemName: icon)
                .font(.system(size: 24))
                .foregroundStyle(VPTheme.textTertiary)
            Text(title)
                .font(VPTheme.font(14, weight: .semibold))
            Text(detail)
                .font(VPTheme.mono(8.5))
                .foregroundStyle(VPTheme.textSecondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 48)
    }
}
