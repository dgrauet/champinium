// Modèle de vue : pont mince vers le noyau via les bindings UniFFI C#.
// Aucune logique métier ici — uniquement de l'orchestration d'appels au noyau
// et de l'état d'affichage (équivalent C#/MVVM du NodeModel.swift macOS).
using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Threading.Tasks;
using Champinium.Core; // bindings générés par `just gen-csharp`

namespace Champinium;

/// <summary>Une ligne de contenu affichable, rattachée à son groupe (créateur/feed).</summary>
public sealed class CatalogCid
{
    public string Cid { get; init; } = "";
    public string Title { get; init; } = "";
    public IReadOnlyList<string> Tags { get; init; } = Array.Empty<string>();

    /// <summary>Vrai si CE nœud a épinglé ce manifeste (lot c) — n'a de sens
    /// que dans l'onglet Abonnements ; toujours faux côté Explorer/recherche.</summary>
    public bool IsPinned { get; init; }

    /// <summary>Vrai si le bouton de pin doit être affiché pour cette ligne
    /// (Abonnements uniquement — même décision que le jumeau macOS ; le
    /// gabarit d'item est partagé avec Explorer, où le bouton reste masqué).</summary>
    public bool CanPin { get; init; }

    /// <summary>Libellé principal : le titre, ou le CID si sans titre.</summary>
    public string Display => Title.Length > 0 ? Title : Cid;

    /// <summary>Tags joints pour l'affichage (vide si aucun).</summary>
    public string TagsText => string.Join(" · ", Tags);

    /// <summary>Libellé du bouton pin — "Garder" (non épinglé) / "Oublier" (épinglé).</summary>
    public string PinLabel => IsPinned ? "Oublier" : "Garder";
}

/// <summary>
/// Un groupe par créateur/channel : en-tête (nom, seq, bouton abonnement) +
/// ses contenus. Le bouton S'abonner/Se désabonner vit ICI (au niveau du
/// groupe), jamais sur une ligne de contenu — même décision que le jumeau
/// macOS (ContentView.swift).
/// </summary>
public sealed class ChannelGroup
{
    public string Issuer { get; init; } = "";
    public ulong Seq { get; init; }
    public bool IsSubscribed { get; init; }
    public ObservableCollection<CatalogCid> Items { get; } = new();

    /// <summary>Nom du channel, ou PeerId tronqué si le channel n'a pas de nom.</summary>
    public string DisplayName { get; init; } = "";

    public string SeqText => $"seq {Seq}";

    /// <summary>Couverture du seed proactif pour ce feed (lot c).</summary>
    public uint SeededCount { get; init; }
    public uint TotalCount { get; init; }

    /// <summary>« à jour » si tout est seedé localement, sinon « seed en cours
    /// (x/y) ». Un feed vide (rien à seeder) n'affiche rien.</summary>
    public string SeedStatusText => TotalCount == 0
        ? ""
        : SeededCount == TotalCount
            ? "· à jour"
            : $"· seed en cours ({SeededCount}/{TotalCount})";

    /// <summary>Libellé du bouton — calculé depuis l'état d'abonnement réel
    /// (dans l'onglet Abonnements, toujours vrai par construction).</summary>
    public string SubscribeLabel => IsSubscribed ? "Se désabonner" : "S'abonner";
}

/// <summary>
/// Orchestration des appels au noyau Champinium. Toute la logique vit dans le
/// core Rust : ce VM ne fait qu'enchaîner les appels du contrat et exposer l'état.
/// </summary>
public sealed class NodeViewModel : INotifyPropertyChanged
{
    /// <summary>
    /// Pont vers le callback du contrat : le noyau rappelle hors du thread UI ;
    /// on re-dispatche vers le thread principal, où le VM relit le catalogue.
    /// </summary>
    private sealed class CatalogRefresher : CatalogListener
    {
        private readonly Action _onUpdate;

        public CatalogRefresher(Action onUpdate) => _onUpdate = onUpdate;

        public void OnCatalogUpdated() => _onUpdate();
    }

    /// <summary>Pont vers le callback de seed proactif (lot c) — même patron que
    /// <see cref="CatalogRefresher"/> : re-dispatch, puis le VM relit
    /// `StorageStats()`/le catalogue (la couverture voyage dans FfiCatalogEntry).</summary>
    private sealed class SeedRefresher : SeedListener
    {
        private readonly Action _onUpdate;

        public SeedRefresher(Action onUpdate) => _onUpdate = onUpdate;

        public void OnSeedUpdated() => _onUpdate();
    }

    private ChampiniumNode? _node;
    private Microsoft.UI.Dispatching.DispatcherQueue? _dispatcher;
    private CatalogRefresher? _listener;
    private SeedRefresher? _seedListener;

    /// <summary>Répertoire de la lecture en cours (supprimé au changement de contenu).</summary>
    private string? _currentPlayDir;

    /// <summary>Racine des répertoires de lecture temporaires.</summary>
    private static string PlayRoot => Path.Combine(Path.GetTempPath(), "champinium-play");

    private string _status = "démarrage…";
    public string Status
    {
        get => _status;
        private set => Set(ref _status, value);
    }

    private string _peerId = "";
    public string PeerId
    {
        get => _peerId;
        private set => Set(ref _peerId, value);
    }

    private string _listenAddr = "";
    public string ListenAddr
    {
        get => _listenAddr;
        private set => Set(ref _listenAddr, value);
    }

    /// <summary>Multiaddr du pair saisi par l'utilisateur (liaison TextBox).</summary>
    private string _peerField = "";
    public string PeerField
    {
        get => _peerField;
        set => Set(ref _peerField, value);
    }

    /// <summary>Lien ou PeerId collé pour s'abonner (liaison TextBox, onglet Explorer).</summary>
    private string _channelLinkField = "";
    public string ChannelLinkField
    {
        get => _channelLinkField;
        set => Set(ref _channelLinkField, value);
    }

    /// <summary>Catalogue restreint aux émetteurs souscrits (`catalog_subscribed()` —
    /// aucun filtrage côté C#, le core fait le tri).</summary>
    public ObservableCollection<ChannelGroup> SubscribedGroups { get; } = new();

    /// <summary>Catalogue complet du réseau (`catalog()`) — onglet Explorer.</summary>
    public ObservableCollection<ChannelGroup> ExploreGroups { get; } = new();

    /// <summary>Résultats de la recherche locale (titres/tags) — remplace les deux
    /// listes ci-dessus tant que <see cref="SearchQuery"/> n'est pas vide.</summary>
    public ObservableCollection<CatalogCid> SearchResults { get; } = new();

    /// <summary>Requête de recherche locale (liaison TextBox) ; vide = catalogues normaux.</summary>
    private string _searchQuery = "";
    public string SearchQuery
    {
        get => _searchQuery;
        set
        {
            Set(ref _searchQuery, value);
            RefreshCatalog();
        }
    }

    /// <summary>PeerIds actuellement souscrits (pour calculer l'état des boutons Explorer).</summary>
    private HashSet<string> _subscriptions = new();

    /// <summary>`(octets_utilisés, quota_octets)` du seed proactif de ce nœud (lot c).</summary>
    private FfiStorageStats _storageStats = new(0, 0);
    public FfiStorageStats StorageStats
    {
        get => _storageStats;
        private set
        {
            if (Equals(_storageStats, value))
            {
                return;
            }
            _storageStats = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(StorageStats)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(StorageStatsText)));
        }
    }

    /// <summary>Quota de seeding saisi par l'utilisateur, en Go (liaison TextBox).</summary>
    private string _quotaField = "";
    public string QuotaField
    {
        get => _quotaField;
        set => Set(ref _quotaField, value);
    }

    /// <summary>Affichage humain de l'usage courant (liaison TextBlock du popover de réglages).</summary>
    public string StorageStatsText =>
        $"Utilisé : {GigabytesText(StorageStats.usedBytes)} Go / {GigabytesText(StorageStats.quotaBytes)} Go";

    /// <summary>Émis quand un abonnement/désabonnement échoue ou réussit — la vue
    /// affiche un message court (distinct des erreurs réseau pour un refus de
    /// modération, distinct des soucis réseau).</summary>
    private string? _subscriptionStatus;
    public string? SubscriptionStatus
    {
        get => _subscriptionStatus;
        private set => Set(ref _subscriptionStatus, value);
    }

    /// <summary>
    /// Émis quand un média est prêt à être lu : la vue branche le chemin du
    /// playlist sur le MediaPlayerElement. (Le VM ne référence pas l'UI média.)
    /// </summary>
    public event Action<string>? PlaybackReady;

    /// <summary>Ouvre le nœud sous le répertoire de données durable de l'OS et commence à écouter.</summary>
    public async Task StartAsync()
    {
        // Purge (best-effort) les répertoires de lecture des exécutions
        // précédentes : ils ne servent qu'à la session en cours.
        try
        {
            Directory.Delete(PlayRoot, recursive: true);
        }
        catch (IOException) { } // couvre aussi DirectoryNotFoundException
        catch (UnauthorizedAccessException) { }

        try
        {
            // Répertoire durable choisi par le noyau (%LocalAppData%\Champinium
            // sur Windows) — jamais un temporaire, pour préserver le PeerId.
            var dir = ChampiniumCoreMethods.DefaultDataDir();
            Directory.CreateDirectory(dir);

            // Fonction libre `open_node` du contrat : exposée par uniffi-bindgen-cs
            // dans la classe statique de module `ChampiniumCoreMethods` (dérivée du
            // namespace UniFFI `champinium_core`), sous le namespace C# Champinium.Core.
            var node = await ChampiniumCoreMethods.OpenNode(dir);
            _node = node;
            PeerId = node.PeerId();
            ListenAddr = await node.Listen("/ip4/0.0.0.0/tcp/0");

            // Rafraîchissement réactif : le noyau notifie chaque changement du
            // catalogue (plus de délai gossip codé en dur). StartAsync est
            // appelé depuis le thread UI, dont on capture le dispatcher ici.
            _dispatcher = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
            _listener = new CatalogRefresher(
                () => _dispatcher?.TryEnqueue(RefreshCatalog));
            await node.SetCatalogListener(_listener);

            _seedListener = new SeedRefresher(
                () => _dispatcher?.TryEnqueue(RefreshCatalog));
            await node.SetSeedListener(_seedListener);

            Status = "nœud en ligne";
            RefreshCatalog();
            QuotaField = GigabytesText(StorageStats.quotaBytes);
        }
        catch (Exception ex)
        {
            Status = $"erreur d'ouverture: {ex.Message}";
        }
    }

    /// <summary>
    /// Se connecte au pair saisi ; le catalogue se rafraîchit tout seul quand
    /// les feeds arrivent (voir <see cref="CatalogRefresher"/>).
    /// </summary>
    public async Task ConnectAsync()
    {
        if (_node is null || string.IsNullOrWhiteSpace(PeerField))
        {
            return;
        }

        try
        {
            await _node.Connect(PeerField);
            Status = "connecté à un pair";
        }
        catch (Exception ex)
        {
            Status = $"connexion: {ex.Message}";
        }
    }

    /// <summary>
    /// Met à jour les deux catalogues (Abonnements + Explorer) et l'état des
    /// abonnements depuis le noyau — aucun filtrage côté C#, `catalog_subscribed()`
    /// et `catalog()` viennent déjà triés du core.
    /// </summary>
    public void RefreshCatalog()
    {
        if (_node is null)
        {
            return;
        }

        _subscriptions = _node.Subscriptions().ToHashSet();

        if (!string.IsNullOrWhiteSpace(SearchQuery))
        {
            SearchResults.Clear();
            var hits = _node.Search(SearchQuery);
            foreach (var hit in hits)
            {
                SearchResults.Add(new CatalogCid
                {
                    Cid = hit.cid,
                    Title = hit.title,
                    Tags = hit.tags,
                });
            }
            Status = $"recherche: {hits.Count} résultat(s)";
            return;
        }

        StorageStats = _node.StorageStats();
        // Abonnements seul montre le pin : épingler un contenu hors abonnement
        // n'a pas de sens dans cette UI (même décision que le jumeau macOS).
        Fill(SubscribedGroups, _node.CatalogSubscribed(), showPin: true);
        Fill(ExploreGroups, _node.Catalog(), showPin: false);
        Status = $"catalogue: {ExploreGroups.Count} créateur(s), {SubscribedGroups.Count} souscrit(s)";
    }

    private void Fill(ObservableCollection<ChannelGroup> target, IReadOnlyList<FfiCatalogEntry> entries, bool showPin)
    {
        target.Clear();
        foreach (var entry in entries)
        {
            var group = new ChannelGroup
            {
                Issuer = entry.issuer,
                Seq = entry.seq,
                IsSubscribed = _subscriptions.Contains(entry.issuer),
                DisplayName = entry.channel.name.Length > 0 ? entry.channel.name : Truncate(entry.issuer),
                SeededCount = entry.seededCount,
                TotalCount = entry.totalCount,
            };
            foreach (var item in entry.items)
            {
                group.Items.Add(new CatalogCid
                {
                    Cid = item.cid,
                    Title = item.title,
                    Tags = item.tags,
                    IsPinned = showPin && entry.pinned.Contains(item.cid),
                    CanPin = showPin,
                });
            }
            target.Add(group);
        }
    }

    private static string Truncate(string peerId) =>
        peerId.Length > 14 ? $"{peerId[..8]}…{peerId[^4..]}" : peerId;

    /// <summary>S'abonne via le lien/PeerId collé dans <see cref="ChannelLinkField"/>.</summary>
    public async Task SubscribeByLinkAsync()
    {
        var link = ChannelLinkField.Trim();
        if (link.Length == 0)
        {
            return;
        }

        try
        {
            await _node!.SubscribeChannel(link);
            ChannelLinkField = "";
            SubscriptionStatus = "abonné";
            RefreshCatalog();
        }
        catch (Exception ex)
        {
            SubscriptionStatus = DescribeSubscriptionError(ex);
        }
    }

    /// <summary>
    /// Bascule l'abonnement d'un émetteur (bouton par groupe/channel, disponible
    /// depuis Abonnements ET Explorer — même décision que le jumeau macOS).
    /// </summary>
    public async Task ToggleSubscriptionAsync(string peerId, bool currentlySubscribed)
    {
        if (_node is null)
        {
            return;
        }

        try
        {
            if (currentlySubscribed)
            {
                await _node.UnsubscribeChannel(peerId);
                SubscriptionStatus = "désabonné";
            }
            else
            {
                await _node.SubscribeChannel(peerId);
                SubscriptionStatus = "abonné";
            }
            RefreshCatalog();
        }
        catch (Exception ex)
        {
            SubscriptionStatus = DescribeSubscriptionError(ex);
        }
    }

    /// <summary>Définit le quota de seed depuis <see cref="QuotaField"/> (Go).
    /// Ignore une saisie non numérique plutôt que de planter.</summary>
    public async Task SetSeedQuotaAsync()
    {
        if (_node is null || !double.TryParse(
                QuotaField.Replace(',', '.'),
                System.Globalization.NumberStyles.Float,
                System.Globalization.CultureInfo.InvariantCulture,
                out var gigabytes))
        {
            return;
        }

        try
        {
            var bytes = (ulong)Math.Max(0, gigabytes * 1_000_000_000);
            await _node.SetSeedQuota(bytes);
            SubscriptionStatus = "quota mis à jour";
            RefreshCatalog();
        }
        catch (Exception)
        {
            SubscriptionStatus = "quota: erreur";
        }
    }

    /// <summary>Bascule l'épinglage d'un manifeste (bouton par publication,
    /// Abonnements uniquement — voir <see cref="CatalogCid.CanPin"/>).</summary>
    public async Task TogglePinAsync(string manifestCid, bool currentlyPinned)
    {
        if (_node is null)
        {
            return;
        }

        try
        {
            if (currentlyPinned)
            {
                await _node.UnpinContent(manifestCid);
            }
            else
            {
                await _node.PinContent(manifestCid);
            }
            RefreshCatalog();
        }
        catch (Exception)
        {
            SubscriptionStatus = "épinglage: erreur";
        }
    }

    private static string GigabytesText(ulong bytes) =>
        (bytes / 1_000_000_000.0).ToString("F1", System.Globalization.CultureInfo.InvariantCulture);

    /// <summary>Erreur typée du contrat : un refus de modération est un blocage
    /// volontaire, présenté comme tel (distinct d'une panne réseau).</summary>
    private static string DescribeSubscriptionError(Exception ex) => ex switch
    {
        FfiException.InvalidInput => "saisie invalide",
        FfiException.Moderated => "contenu bloqué par la modération",
        _ => "erreur réseau",
    };

    /// <summary>Lien partageable du channel de ce nœud, pour le bouton "copier".</summary>
    public string? MyChannelLink()
    {
        if (_node is null)
        {
            return null;
        }
        try
        {
            return _node.ChannelLink(_node.PeerId());
        }
        catch (Exception)
        {
            return null;
        }
    }

    /// <summary>Récupère un HLS (manifeste) et signale qu'il est prêt à lire.</summary>
    public async Task PlayAsync(string manifestCid)
    {
        if (_node is null)
        {
            return;
        }

        // Supprime (best-effort) le répertoire de la lecture précédente : le
        // lecteur peut encore verrouiller un segment ; le reliquat éventuel est
        // repris par la purge du prochain démarrage.
        if (_currentPlayDir is not null)
        {
            try
            {
                Directory.Delete(_currentPlayDir, recursive: true);
            }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
            _currentPlayDir = null;
        }

        try
        {
            var outDir = Path.Combine(PlayRoot, Guid.NewGuid().ToString());
            Directory.CreateDirectory(outDir);

            var playlist = await _node.FetchHls(manifestCid, outDir);
            _currentPlayDir = outDir;
            Status = "lecture en cours";
            PlaybackReady?.Invoke(playlist);
        }
        // Erreur typée du contrat : un refus de modération est un blocage
        // volontaire, présenté comme tel (pas comme une panne technique).
        catch (FfiException.Moderated)
        {
            Status = "contenu bloqué par la modération";
        }
        catch (Exception ex)
        {
            Status = $"lecture: {ex.Message}";
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (Equals(field, value))
        {
            return;
        }
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
    }
}
