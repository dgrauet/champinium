// Code-behind de la fenêtre principale. Câble les boutons au modèle de vue et
// branche le lecteur Media Foundation. Présentation uniquement : toute la logique
// vit dans le noyau Rust (appelé via le NodeViewModel).
using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;
using Windows.Media.Core;
using Windows.Media.Playback;

namespace Champinium;

public sealed partial class MainWindow : Window
{
    /// <summary>Modèle de vue — posé comme DataContext de la Grid racine (voir
    /// constructeur) ; tous les `{Binding …}` de MainWindow.xaml résolvent
    /// dessus (Binding classique partout, pas de x:Bind — voir MainWindow.xaml
    /// pour la raison : un x:Bind dans un DataTemplate en ressource nommée,
    /// partagée par plusieurs ListView à l'intérieur d'un Pivot, générait un
    /// connecteur ancré sur la Window plutôt que sur l'item).</summary>
    public NodeViewModel Model { get; } = new();

    /// <summary>Index de l'onglet Explorer dans <c>CatalogPivot</c>.</summary>
    private const int ExplorerPivotIndex = 1;

    /// <summary>Vrai pendant qu'on revient nous-mêmes sur Abonnements après un
    /// refus de l'avertissement — évite de redéclencher le dialogue en boucle.</summary>
    private bool _revertingPivotSelection;

    public MainWindow()
    {
        InitializeComponent();

        // Root.DataContext = Model : source de résolution de tous les
        // `{Binding …}` du XAML (Binding classique intégral, voir plus haut).
        Root.DataContext = Model;

        // Quand un média est prêt, branche son playlist HLS sur le lecteur.
        Model.PlaybackReady += OnPlaybackReady;

        // Au lancement : openNode → listen (équivalent du .task macOS).
        DispatcherQueue.TryEnqueue(async () => await Model.StartAsync());
    }

    private async void OnConnectClick(object sender, RoutedEventArgs e)
    {
        await Model.ConnectAsync();
    }

    private void OnRefreshClick(object sender, RoutedEventArgs e)
    {
        Model.RefreshCatalog();
    }

    private async void OnPlayClick(object sender, RoutedEventArgs e)
    {
        // Le CID est porté par le Tag du bouton (DataTemplate du catalogue).
        if (sender is Button { Tag: string cid })
        {
            await Model.PlayAsync(cid);
        }
    }

    /// <summary>
    /// Premier accès à l'onglet Explorer : avertissement sur le contenu non
    /// curé, mémorisé dans <see cref="LocalSettings"/>. Refus → retour sur
    /// Abonnements.
    /// </summary>
    private async void OnPivotSelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_revertingPivotSelection || CatalogPivot.SelectedIndex != ExplorerPivotIndex)
        {
            return;
        }

        if (LocalSettings.ExplorerAccepted)
        {
            return;
        }

        var dialog = new ContentDialog
        {
            Title = "Explorer",
            Content = "Contenu non curé venant du réseau ouvert, filtré uniquement par les denylists.",
            PrimaryButtonText = "Continuer",
            CloseButtonText = "Annuler",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = Content.XamlRoot,
        };

        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary)
        {
            LocalSettings.ExplorerAccepted = true;
        }
        else
        {
            _revertingPivotSelection = true;
            CatalogPivot.SelectedIndex = 0;
            _revertingPivotSelection = false;
        }
    }

    private async void OnSubscribeByLinkClick(object sender, RoutedEventArgs e)
    {
        await Model.SubscribeByLinkAsync();
    }

    /// <summary>
    /// Bascule l'abonnement d'un groupe (créateur/channel). Le bouton est dans un
    /// gabarit lié par `Binding` classique (pas `x:Bind` — voir MainWindow.xaml) :
    /// le conteneur d'item pose le <see cref="ChannelGroup"/> comme DataContext,
    /// c'est là qu'on le relit (état actuel), pas via Tag.
    /// </summary>
    private async void OnToggleSubscriptionClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: ChannelGroup group })
        {
            await Model.ToggleSubscriptionAsync(group.Issuer, group.IsSubscribed);
        }
    }

    private async void OnSaveSeedQuotaClick(object sender, RoutedEventArgs e)
    {
        await Model.SetSeedQuotaAsync();
    }

    /// <summary>
    /// Bloque un channel (bouton d'en-tête, Explorer uniquement — voir
    /// <see cref="ChannelGroup.CanBlock"/>). Confirmation simple, même patron
    /// que l'avertissement Explorer (<see cref="OnPivotSelectionChanged"/>) :
    /// action irréversible visuellement (le channel disparaît du catalogue),
    /// même si le blocage reste réversible depuis « Channels bloqués ».
    /// </summary>
    private async void OnBlockClick(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { DataContext: ChannelGroup group })
        {
            return;
        }

        var dialog = new ContentDialog
        {
            Title = "Bloquer ce channel ?",
            Content = $"« {group.DisplayName} » disparaîtra du catalogue. Réversible depuis « Channels bloqués ».",
            PrimaryButtonText = "Bloquer",
            CloseButtonText = "Annuler",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = Content.XamlRoot,
        };

        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary)
        {
            await Model.BlockChannelAsync(group.Issuer);
        }
    }

    /// <summary>
    /// Débloque un channel (bouton de la liste « Channels bloqués », voir
    /// <see cref="NodeViewModel.BlockedChannels"/>). Même patron d'accès au
    /// DataContext que <see cref="OnBlockClick"/>.
    /// </summary>
    private async void OnUnblockClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: BlockedChannel blocked })
        {
            await Model.UnblockChannelAsync(blocked.PeerId);
        }
    }

    /// <summary>
    /// Bascule l'épinglage d'une publication. Même patron que
    /// <see cref="OnToggleSubscriptionClick"/> : le conteneur d'item du gabarit
    /// partagé pose le <see cref="CatalogCid"/> comme DataContext.
    /// </summary>
    private async void OnTogglePinClick(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: CatalogCid cid })
        {
            await Model.TogglePinAsync(cid.Cid, cid.IsPinned);
        }
    }

    private void OnCopyMyLinkClick(object sender, RoutedEventArgs e)
    {
        var link = Model.MyChannelLink();
        if (string.IsNullOrEmpty(link))
        {
            return;
        }

        var package = new DataPackage();
        package.SetText(link);
        Clipboard.SetContent(package);
    }

    /// <summary>Reçoit le chemin du index.m3u8 reconstruit et lance la lecture.</summary>
    private void OnPlaybackReady(string playlistPath)
    {
        // Repasse sur le thread UI : l'événement peut venir d'un await arrière-plan.
        DispatcherQueue.TryEnqueue(() =>
        {
            // Chemin de fichier local → URI file:// pour MediaSource.
            var uri = new Uri(playlistPath);
            Player.Source = MediaSource.CreateFromUri(uri);

            if (Player.MediaPlayer is MediaPlayer mp)
            {
                mp.AutoPlay = true;
                mp.Play();
            }
        });
    }
}
