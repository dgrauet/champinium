// Vue principale : barre de connexion, catalogue, et lecteur AVPlayer.
import AVKit
import SwiftUI
import ChampiniumCore

struct ContentView: View {
    @StateObject private var model = NodeModel()
    @State private var peerField: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            connectBar
            Divider()
            catalogList
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

    private var catalogList: some View {
        List {
            ForEach(Array(model.entries.enumerated()), id: \.offset) { _, entry in
                Section("créateur \(entry.issuer) — seq \(entry.seq)") {
                    ForEach(entry.cids, id: \.self) { cid in
                        HStack {
                            Text(cid).font(.system(.caption, design: .monospaced)).lineLimit(1)
                            Spacer()
                            Button("Lire") { Task { await model.play(manifestCid: cid) } }
                        }
                    }
                }
            }
        }
    }
}
