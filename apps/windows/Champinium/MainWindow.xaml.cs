// Code-behind de la fenêtre principale. Câble les boutons au modèle de vue et
// branche le lecteur Media Foundation. Présentation uniquement : toute la logique
// vit dans le noyau Rust (appelé via le NodeViewModel).
using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Media.Core;
using Windows.Media.Playback;

namespace Champinium;

public sealed partial class MainWindow : Window
{
    /// <summary>Modèle de vue lié au XAML (x:Bind).</summary>
    public NodeViewModel Model { get; } = new();

    public MainWindow()
    {
        InitializeComponent();

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
