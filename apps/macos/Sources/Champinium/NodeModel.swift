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

/// Pont vers le callback de seed proactif (lot c) : même patron que
/// `CatalogRefresher` — re-dispatch vers le thread principal, où le modèle
/// relit `storageStats()` et le catalogue (la couverture de seed voyage dans
/// `FfiCatalogEntry`).
private final class SeedRefresher: SeedListener {
    private let onUpdate: @Sendable () -> Void

    init(onUpdate: @escaping @Sendable () -> Void) {
        self.onUpdate = onUpdate
    }

    func onSeedUpdated() {
        onUpdate()
    }
}

@MainActor
final class NodeModel: ObservableObject {
    @Published var status: String = "démarrage…"
    @Published var peerId: String = ""
    @Published var listenAddr: String = ""
    @Published var entries: [FfiCatalogEntry] = []
    @Published var subscribedEntries: [FfiCatalogEntry] = []
    @Published var subscriptions: Set<String> = []
    @Published var blockedChannels: [String] = []
    @Published var searchHits: [FfiSearchHit] = []
    @Published var storageStats = FfiStorageStats(usedBytes: 0, quotaBytes: 0)
    @Published var player: AVPlayer?

    private var node: ChampiniumNode?
    private var listener: CatalogListener?
    private var seedListener: SeedListener?
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
            let seedRefresher = SeedRefresher { [weak self] in
                Task { @MainActor in self?.refreshCatalog() }
            }
            seedListener = seedRefresher
            await node.setSeedListener(listener: seedRefresher)
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

    /// Met à jour les deux listes (Explorer + Abonnements) et les abonnements
    /// courants depuis le noyau.
    func refreshCatalog() {
        entries = node?.catalog() ?? []
        subscribedEntries = node?.catalogSubscribed() ?? []
        subscriptions = Set(node?.subscriptions() ?? [])
        blockedChannels = node?.blockedChannels() ?? []
        storageStats = node?.storageStats() ?? FfiStorageStats(usedBytes: 0, quotaBytes: 0)
        status = "catalogue: \(entries.count) créateur(s)"
    }

    /// Définit le quota de seed proactif en gigaoctets (arrondi à l'octet).
    func setSeedQuotaGB(_ gigabytes: Double) async throws {
        guard let node else { return }
        let bytes = UInt64(max(0, gigabytes) * 1_000_000_000)
        try await node.setSeedQuota(bytes: bytes)
        refreshCatalog()
    }

    /// Épingle un manifeste (exempté d'éviction par le seed proactif).
    func pin(_ manifestCid: String) async throws {
        guard let node else { return }
        try await node.pinContent(manifestCid: manifestCid)
        refreshCatalog()
    }

    /// Retire l'épinglage d'un manifeste.
    func unpin(_ manifestCid: String) async throws {
        guard let node else { return }
        try await node.unpinContent(manifestCid: manifestCid)
        refreshCatalog()
    }

    /// Lien partageable du channel de ce nœud.
    func myChannelLink() -> String? {
        guard let node else { return nil }
        return try? node.channelLink(peerId: node.peerId())
    }

    /// S'abonne à un channel via un lien `champinium://channel/<clé>` ou un
    /// PeerId nu. Rafraîchit le catalogue en cas de succès.
    func subscribeChannel(_ linkOrPeerId: String) async throws {
        guard let node else { return }
        try await node.subscribeChannel(linkOrPeerId: linkOrPeerId)
        refreshCatalog()
    }

    /// Résout un aperçu de channel par lien ou PeerId nu, sans s'abonner —
    /// alimente la feuille d'aperçu (voir `ContentView`).
    func resolveChannel(_ linkOrPeerId: String) async throws -> FfiChannelPreview {
        guard let node else { throw FfiError.Internal(msg: "nœud non initialisé") }
        return try await node.resolveChannel(linkOrPeerId: linkOrPeerId)
    }

    /// Se désabonne d'un émetteur. Rafraîchit le catalogue en cas de succès.
    func unsubscribeChannel(_ peerId: String) async throws {
        guard let node else { return }
        try await node.unsubscribeChannel(peerId: peerId)
        refreshCatalog()
    }

    /// Bloque un channel localement (lien ou PeerId nu). Le channel disparaît
    /// du catalogue via le rafraîchissement réactif du `CatalogListener`
    /// (`refreshCatalog()` explicite ici en plus, pour un retour immédiat).
    func blockChannel(_ linkOrPeerId: String) async throws {
        guard let node else { return }
        try await node.blockChannel(linkOrPeerId: linkOrPeerId)
        refreshCatalog()
    }

    /// Débloque un channel bloqué localement.
    func unblockChannel(_ peerId: String) async throws {
        guard let node else { return }
        try await node.unblockChannel(peerId: peerId)
        refreshCatalog()
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
