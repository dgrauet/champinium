// Vue principale : barre de connexion, catalogue, et lecteur AVPlayer.
import AVKit
import ChampiniumCore
import SwiftUI

struct ContentView: View {
    @StateObject private var model = NodeModel()
    @State private var peerField: String = ""
    @State private var searchQuery: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            connectBar
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
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Champinium").font(.title2).bold()
            Text(model.status).font(.caption).foregroundStyle(.secondary)
            if !model.peerId.isEmpty {
                Text("PeerId : \(model.peerId)").font(.caption2).foregroundStyle(.tertiary)
            }
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

    private var searchBar: some View {
        TextField("Rechercher (titre ou tag)…", text: $searchQuery)
            .textFieldStyle(.roundedBorder)
            .onChange(of: searchQuery, perform: { model.search($0) })
    }

    private var catalogList: some View {
        List {
            ForEach(model.entries, id: \.issuer) { entry in
                Section("créateur \(entry.issuer) — seq \(entry.seq)") {
                    ForEach(entry.items, id: \.cid) { item in
                        contentRow(
                            title: item.title, tags: item.tags, cid: item.cid
                        )
                    }
                }
            }
        }
    }

    private var searchResults: some View {
        List {
            ForEach(model.searchHits, id: \.cid) { hit in
                contentRow(title: hit.title, tags: hit.tags, cid: hit.cid)
            }
            if model.searchHits.isEmpty {
                Text("aucun résultat").foregroundStyle(.secondary)
            }
        }
    }

    /// Une ligne de contenu : titre (ou CID si sans titre), tags, bouton Lire.
    private func contentRow(title: String, tags: [String], cid: String) -> some View {
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
            Button("Lire") { Task { await model.play(manifestCid: cid) } }
        }
    }
}
