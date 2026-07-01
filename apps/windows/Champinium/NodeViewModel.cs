// Modèle de vue : pont mince vers le noyau via les bindings UniFFI C#.
// Aucune logique métier ici — uniquement de l'orchestration d'appels au noyau
// et de l'état d'affichage (équivalent C#/MVVM du NodeModel.swift macOS).
using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using System.Runtime.CompilerServices;
using System.Threading.Tasks;
using Champinium.Core; // bindings générés par `just gen-csharp`

namespace Champinium;

/// <summary>Une ligne affichable : un CID rattaché à son créateur/feed.</summary>
public sealed class CatalogCid
{
    public string Issuer { get; init; } = "";
    public ulong Seq { get; init; }
    public string Cid { get; init; } = "";

    /// <summary>Libellé court du créateur, pour l'en-tête de section.</summary>
    public string Header => $"créateur {Issuer} — seq {Seq}";
}

/// <summary>
/// Orchestration des appels au noyau Champinium. Toute la logique vit dans le
/// core Rust : ce VM ne fait qu'enchaîner les appels du contrat et exposer l'état.
/// </summary>
public sealed class NodeViewModel : INotifyPropertyChanged
{
    private ChampiniumNode? _node;

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

    /// <summary>Catalogue aplati (un élément par CID) pour la liste.</summary>
    public ObservableCollection<CatalogCid> Entries { get; } = new();

    /// <summary>
    /// Émis quand un média est prêt à être lu : la vue branche le chemin du
    /// playlist sur le MediaPlayerElement. (Le VM ne référence pas l'UI média.)
    /// </summary>
    public event Action<string>? PlaybackReady;

    /// <summary>Ouvre le nœud sous le répertoire de données durable de l'OS et commence à écouter.</summary>
    public async Task StartAsync()
    {
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
            Status = "nœud en ligne";
        }
        catch (Exception ex)
        {
            Status = $"erreur d'ouverture: {ex.Message}";
        }
    }

    /// <summary>Se connecte au pair saisi, puis rafraîchit le catalogue.</summary>
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
            // Laisse au gossip le temps de propager les feeds avant de relire.
            await Task.Delay(2000);
            RefreshCatalog();
        }
        catch (Exception ex)
        {
            Status = $"connexion: {ex.Message}";
        }
    }

    /// <summary>Met à jour la liste depuis le catalogue reconstruit localement.</summary>
    public void RefreshCatalog()
    {
        Entries.Clear();
        var issuers = 0;
        if (_node is not null)
        {
            foreach (var entry in _node.Catalog())
            {
                issuers++;
                foreach (var cid in entry.Cids)
                {
                    Entries.Add(new CatalogCid
                    {
                        Issuer = entry.Issuer,
                        Seq = entry.Seq,
                        Cid = cid,
                    });
                }
            }
        }
        Status = $"catalogue: {issuers} créateur(s)";
    }

    /// <summary>Récupère un HLS (manifeste) et signale qu'il est prêt à lire.</summary>
    public async Task PlayAsync(string manifestCid)
    {
        if (_node is null)
        {
            return;
        }

        try
        {
            var outDir = Path.Combine(
                Path.GetTempPath(), "champinium-play", Guid.NewGuid().ToString());
            Directory.CreateDirectory(outDir);

            var playlist = await _node.FetchHls(manifestCid, outDir);
            Status = "lecture en cours";
            PlaybackReady?.Invoke(playlist);
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
