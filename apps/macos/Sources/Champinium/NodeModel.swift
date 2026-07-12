// Modèle d'application : pont mince vers le noyau via les bindings UniFFI.
// Aucune logique métier ici — uniquement de l'orchestration d'appels au noyau
// et de l'état d'affichage.
import AVFoundation
import ChampiniumCore
import Foundation

/// Pont vers le callback du contrat : le noyau rappelle `onCatalogUpdated` hors
/// du thread UI ; on re-dispatche vers le thread principal, où le modèle relit
/// l'instantané `catalog()`.
private final class CatalogRefresher: CatalogListener {
    private let onUpdate: @Sendable () -> Void

    init(onUpdate: @escaping @Sendable () -> Void) {
        self.onUpdate = onUpdate
    }

    func onCatalogUpdated() {
        onUpdate()
    }
}

@MainActor
final class NodeModel: ObservableObject {
    @Published var status: String = "démarrage…"
    @Published var peerId: String = ""
    @Published var listenAddr: String = ""
    @Published var entries: [FfiCatalogEntry] = []
    @Published var searchHits: [FfiSearchHit] = []
    @Published var player: AVPlayer?

    private var node: ChampiniumNode?
    private var listener: CatalogListener?
    /// Répertoire de la lecture en cours (supprimé au changement de contenu).
    private var currentPlayDir: String?

    /// Racine des répertoires de lecture temporaires.
    private var playRoot: String {
        NSTemporaryDirectory() + "champinium-play"
    }

    /// Ouvre le nœud, commence à écouter et s'abonne aux mises à jour du
    /// catalogue (rafraîchissement réactif, pas de délai gossip codé en dur).
    func start() async {
        // Purge les répertoires de lecture des exécutions précédentes (ils ne
        // servent qu'à la session en cours et s'accumuleraient sinon).
        try? FileManager.default.removeItem(atPath: playRoot)
        do {
            // Répertoire durable par OS (jamais le tmp : sinon perte du PeerId
            // et régression du seq de feed au nettoyage du système).
            let dir = defaultDataDir()
            let node = try await openNode(dataDir: dir)
            self.node = node
            peerId = node.peerId()
            listenAddr = try await node.listen(addr: "/ip4/0.0.0.0/tcp/0")
            let refresher = CatalogRefresher { [weak self] in
                Task { @MainActor in self?.refreshCatalog() }
            }
            listener = refresher
            await node.setCatalogListener(listener: refresher)
            status = "nœud en ligne"
        } catch {
            status = "erreur d'ouverture: \(error)"
        }
    }

    /// Se connecte à un pair ; le catalogue se rafraîchit tout seul quand les
    /// feeds arrivent (voir `CatalogRefresher`).
    func connect(to peer: String) async {
        guard let node, !peer.isEmpty else { return }
        do {
            try await node.connect(peer: peer)
            status = "connecté à un pair"
        } catch {
            status = "connexion: \(error)"
        }
    }

    /// Met à jour la liste depuis le catalogue reconstruit localement.
    func refreshCatalog() {
        entries = node?.catalog() ?? []
        status = "catalogue: \(entries.count) créateur(s)"
    }

    /// Recherche locale (titres/tags du catalogue) — la logique vit dans le core.
    func search(_ query: String) {
        searchHits = query.isEmpty ? [] : (node?.search(query: query) ?? [])
    }

    /// Récupère et lit un contenu (manifeste HLS) via AVPlayer. Le répertoire
    /// de la lecture précédente est supprimé au passage (pas d'accumulation).
    func play(manifestCid: String) async {
        guard let node else { return }
        if let previous = currentPlayDir {
            player?.pause()
            player = nil
            try? FileManager.default.removeItem(atPath: previous)
            currentPlayDir = nil
        }
        do {
            let out = playRoot + "/" + UUID().uuidString
            let playlist = try await node.fetchHls(manifestCid: manifestCid, outDir: out)
            currentPlayDir = out
            let player = AVPlayer(url: URL(fileURLWithPath: playlist))
            self.player = player
            player.play()
            status = "lecture en cours"
        } catch let e as FfiError {
            // Erreur typée du contrat : un refus de modération est un blocage
            // volontaire, présenté comme tel (pas comme une panne technique).
            switch e {
            case .Moderated:
                self.status = "contenu bloqué par la modération"
            default:
                self.status = "lecture: \(e)"
            }
        } catch {
            status = "lecture: \(error)"
        }
    }
}
