// Vue principale : barre de connexion, catalogue (Abonnements/Explorer), et
// lecteur AVPlayer. Aucune logique métier — le filtrage par abonnement vient
// du core (`catalogSubscribed()`), cette vue ne fait que présenter.
import AppKit
import AVKit
import ChampiniumCore
import SwiftUI

/// Onglet de vue du catalogue — Abonnements par défaut (voir spec §2).
private enum CatalogTab: String, CaseIterable {
    case subscriptions = "Abonnements"
    case explorer = "Explorer"
}

struct ContentView: View {
    @StateObject private var model = NodeModel()
    @State private var peerField: String = ""
    @State private var searchQuery: String = ""
    @State private var tab: CatalogTab = .subscriptions
    @State private var showExplorerWarning = false
    @AppStorage("explorerAccepted") private var explorerAccepted = false
    @State private var channelLinkField: String = ""
    @State private var subscriptionStatus: String?
    @State private var showSeedSettings = false
    @State private var quotaField: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            connectBar
            tabPicker
            if tab == .explorer {
                explorerToolbar
            }
            searchBar
            Divider()
            if searchQuery.isEmpty {
                catalogList
            } else {
                searchResults
            }
            if let player = model.player {
                VideoPlayer(player: player)
                    .frame(minHeight: 220)
                    .cornerRadius(8)
            }
        }
        .padding()
        .task { await model.start() }
        .alert("Explorer", isPresented: $showExplorerWarning) {
            Button("Annuler", role: .cancel) {}
            Button("Continuer") {
                explorerAccepted = true
                tab = .explorer
            }
        } message: {
            Text("Contenu non curé venant du réseau ouvert, filtré uniquement par les denylists.")
        }
    }

    private var header: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Champinium").font(.title2).bold()
                Text(model.status).font(.caption).foregroundStyle(.secondary)
                if !model.peerId.isEmpty {
                    Text("PeerId : \(model.peerId)").font(.caption2).foregroundStyle(.tertiary)
                }
            }
            Spacer()
            Button("Réglages de seed") {
                quotaField = String(format: "%.1f", gigabytes(model.storageStats.quotaBytes))
                showSeedSettings = true
            }
            .font(.caption)
            .popover(isPresented: $showSeedSettings) {
                seedSettingsPopover
            }
        }
    }

    /// Réglage du quota de seeding (GB) + affichage de l'usage courant. Vue
    /// minimale en popover pour rester cohérent avec la vue unique existante.
    private var seedSettingsPopover: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Quota de seeding").font(.headline)
            HStack {
                TextField("Go", text: $quotaField)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 80)
                Text("Go")
                Button("Enregistrer") { Task { await saveSeedQuota() } }
            }
            Text(
                "Utilisé : \(formatGB(model.storageStats.usedBytes)) Go / "
                    + "\(formatGB(model.storageStats.quotaBytes)) Go"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .padding()
        .frame(width: 260)
    }

    private func gigabytes(_ bytes: UInt64) -> Double {
        Double(bytes) / 1_000_000_000
    }

    private func formatGB(_ bytes: UInt64) -> String {
        String(format: "%.1f", gigabytes(bytes))
    }

    private func saveSeedQuota() async {
        guard let gb = Double(quotaField.replacingOccurrences(of: ",", with: ".")) else { return }
        do {
            try await model.setSeedQuotaGB(gb)
            subscriptionStatus = "quota mis à jour"
        } catch {
            subscriptionStatus = "quota: erreur"
        }
    }

    private var connectBar: some View {
        HStack {
            TextField("/ip4/…/tcp/…/p2p/<peerid>", text: $peerField)
                .textFieldStyle(.roundedBorder)
            Button("Connecter") { Task { await model.connect(to: peerField) } }
            Button("Rafraîchir") { model.refreshCatalog() }
        }
    }

    /// Sélecteur segmenté « Abonnements / Explorer ». Le premier passage sur
    /// Explorer déclenche l'avertissement (mémorisé via `explorerAccepted`).
    private var tabPicker: some View {
        Picker("Vue", selection: $tab) {
            ForEach(CatalogTab.allCases, id: \.self) { candidate in
                Text(candidate.rawValue).tag(candidate)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .onChange(of: tab) { newValue in
            guard newValue == .explorer, !explorerAccepted else { return }
            tab = .subscriptions
            showExplorerWarning = true
        }
    }

    /// Abonnement par lien + copie du lien de mon propre channel — visibles
    /// uniquement dans l'onglet Explorer.
    private var explorerToolbar: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                TextField("Coller un lien de channel…", text: $channelLinkField)
                    .textFieldStyle(.roundedBorder)
                Button("S'abonner") { Task { await subscribeByLink() } }
                    .disabled(channelLinkField.trimmingCharacters(in: .whitespacesAndNewlines)
                        .isEmpty)
            }
            Button("Copier le lien de mon channel") { copyMyChannelLink() }
            if let subscriptionStatus {
                Text(subscriptionStatus).font(.caption2).foregroundStyle(.secondary)
            }
        }
    }

    private var searchBar: some View {
        TextField("Rechercher (titre ou tag)…", text: $searchQuery)
            .textFieldStyle(.roundedBorder)
            .onChange(of: searchQuery, perform: { model.search($0) })
    }

    /// Liste courante selon l'onglet — Abonnements ou Explorer, toutes deux
    /// rafraîchies ensemble par `NodeModel.refreshCatalog()`.
    private var catalogList: some View {
        List {
            ForEach(currentEntries, id: \.issuer) { entry in
                Section(header: channelHeader(for: entry)) {
                    ForEach(entry.items, id: \.cid) { item in
                        contentRow(
                            title: item.title, tags: item.tags, cid: item.cid,
                            isPinned: tab == .subscriptions ? entry.pinned.contains(item.cid) : nil
                        )
                    }
                }
            }
        }
    }

    private var currentEntries: [FfiCatalogEntry] {
        tab == .subscriptions ? model.subscribedEntries : model.entries
    }

    /// En-tête de section : nom du channel (ou PeerId tronqué), seq, et bouton
    /// S'abonner/Se désabonner selon le contexte (Explorer/Abonnements).
    private func channelHeader(for entry: FfiCatalogEntry) -> some View {
        HStack {
            VStack(alignment: .leading, spacing: 1) {
                Text(displayName(for: entry)).font(.subheadline).bold()
                HStack(spacing: 6) {
                    Text("seq \(entry.seq)").font(.caption2).foregroundStyle(.tertiary)
                    Text(seedStatus(for: entry)).font(.caption2).foregroundStyle(.tertiary)
                }
            }
            Spacer()
            subscribeButton(for: entry)
        }
    }

    /// « à jour » si tout est seedé localement, sinon « seed en cours (x/y) ».
    /// Un feed vide (`totalCount == 0`, ex. channel sans publication) n'affiche
    /// pas « à jour » — il n'y a rien à seeder.
    private func seedStatus(for entry: FfiCatalogEntry) -> String {
        guard entry.totalCount > 0 else { return "" }
        if entry.seededCount == entry.totalCount {
            return "· à jour"
        }
        return "· seed en cours (\(entry.seededCount)/\(entry.totalCount))"
    }

    private func displayName(for entry: FfiCatalogEntry) -> String {
        entry.channel.name.isEmpty ? truncated(entry.issuer) : entry.channel.name
    }

    private func truncated(_ peerId: String) -> String {
        guard peerId.count > 14 else { return peerId }
        return String(peerId.prefix(8)) + "…" + String(peerId.suffix(4))
    }

    private func subscribeButton(for entry: FfiCatalogEntry) -> some View {
        let subscribed = model.subscriptions.contains(entry.issuer)
        let isAbonnements = tab == .subscriptions

        // In Abonnements, button is always unsubscribe (all entries are subscribed by definition).
        // In Explorer, button toggles based on subscription status.
        let isCurrentlySubscribed = isAbonnements ? true : subscribed
        let label = isCurrentlySubscribed ? "Se désabonner" : "S'abonner"

        return Button(label) {
            Task {
                await toggleSubscription(
                    peerId: entry.issuer,
                    subscribed: isCurrentlySubscribed
                )
            }
        }
        .font(.caption)
        .buttonStyle(.bordered)
    }

    private var searchResults: some View {
        List {
            ForEach(model.searchHits, id: \.cid) { hit in
                contentRow(title: hit.title, tags: hit.tags, cid: hit.cid, isPinned: nil)
            }
            if model.searchHits.isEmpty {
                Text("aucun résultat").foregroundStyle(.secondary)
            }
        }
    }

    /// Une ligne de contenu : titre (ou CID si sans titre), tags, bouton Lire,
    /// et bouton Garder/Oublier (pin) quand `isPinned` est fourni (Abonnements
    /// uniquement — épingler un contenu hors abonnement n'a pas de sens ici).
    private func contentRow(title: String, tags: [String], cid: String,
                            isPinned: Bool?) -> some View
    {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(title.isEmpty ? cid : title)
                    .font(title.isEmpty ? .system(.caption, design: .monospaced) : .body)
                    .lineLimit(1)
                if !tags.isEmpty {
                    Text(tags.joined(separator: " · "))
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
            Spacer()
            if let isPinned {
                Button(isPinned ? "Oublier" : "Garder") {
                    Task { await togglePin(cid: cid, pinned: isPinned) }
                }
                .font(.caption)
                .buttonStyle(.bordered)
            }
            Button("Lire") { Task { await model.play(manifestCid: cid) } }
        }
    }

    private func togglePin(cid: String, pinned: Bool) async {
        do {
            if pinned {
                try await model.unpin(cid)
            } else {
                try await model.pin(cid)
            }
        } catch {
            subscriptionStatus = "épinglage: erreur"
        }
    }

    private func toggleSubscription(peerId: String, subscribed: Bool) async {
        do {
            if subscribed {
                try await model.unsubscribeChannel(peerId)
                subscriptionStatus = "désabonné"
            } else {
                try await model.subscribeChannel(peerId)
                subscriptionStatus = "abonné"
            }
        } catch {
            subscriptionStatus = describeSubscriptionError(error)
        }
    }

    private func subscribeByLink() async {
        let link = channelLinkField.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !link.isEmpty else { return }
        do {
            try await model.subscribeChannel(link)
            channelLinkField = ""
            subscriptionStatus = "abonné"
        } catch {
            subscriptionStatus = describeSubscriptionError(error)
        }
    }

    private func describeSubscriptionError(_ error: Error) -> String {
        guard let ffiError = error as? FfiError else { return "erreur réseau" }
        switch ffiError {
        case .InvalidInput:
            return "saisie invalide"
        case .Moderated:
            return "contenu bloqué par la modération"
        default:
            return "erreur réseau"
        }
    }

    private func copyMyChannelLink() {
        guard let link = model.myChannelLink() else {
            subscriptionStatus = "lien indisponible"
            return
        }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(link, forType: .string)
        subscriptionStatus = "lien copié"
    }
}
