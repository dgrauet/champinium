// Modèle d'application : pont mince vers le noyau via les bindings UniFFI.
// Aucune logique métier ici — uniquement de l'orchestration d'appels au noyau
// et de l'état d'affichage.
import AVFoundation
import Foundation
import ChampiniumCore

@MainActor
final class NodeModel: ObservableObject {
    @Published var status: String = "démarrage…"
    @Published var peerId: String = ""
    @Published var listenAddr: String = ""
    @Published var entries: [FfiCatalogEntry] = []
    @Published var player: AVPlayer?

    private var node: ChampiniumNode?

    /// Ouvre le nœud et commence à écouter.
    func start() async {
        do {
            // Répertoire durable par OS (jamais le tmp : sinon perte du PeerId
            // et régression du seq de feed au nettoyage du système).
            let dir = defaultDataDir()
            let node = try await openNode(dataDir: dir)
            self.node = node
            self.peerId = node.peerId()
            self.listenAddr = try await node.listen(addr: "/ip4/0.0.0.0/tcp/0")
            self.status = "nœud en ligne"
        } catch {
            self.status = "erreur d'ouverture: \(error)"
        }
    }

    /// Se connecte à un pair, puis rafraîchit le catalogue après un court délai.
    func connect(to peer: String) async {
        guard let node, !peer.isEmpty else { return }
        do {
            try await node.connect(peer: peer)
            self.status = "connecté à un pair"
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            refreshCatalog()
        } catch {
            self.status = "connexion: \(error)"
        }
    }

    /// Met à jour la liste depuis le catalogue reconstruit localement.
    func refreshCatalog() {
        entries = node?.catalog() ?? []
        status = "catalogue: \(entries.count) créateur(s)"
    }

    /// Récupère et lit un contenu (manifeste HLS) via AVPlayer.
    func play(manifestCid: String) async {
        guard let node else { return }
        do {
            let out = NSTemporaryDirectory() + "champinium-play/" + UUID().uuidString
            let playlist = try await node.fetchHls(manifestCid: manifestCid, outDir: out)
            let player = AVPlayer(url: URL(fileURLWithPath: playlist))
            self.player = player
            player.play()
            self.status = "lecture en cours"
        } catch {
            self.status = "lecture: \(error)"
        }
    }
}
