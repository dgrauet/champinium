// Point d'entrée de l'application WinUI 3. Crée et active la fenêtre principale.
// Présentation uniquement : aucune logique métier ici.
using Microsoft.UI.Xaml;

namespace Champinium;

public partial class App : Application
{
    private Window? _window;

    public App()
    {
        InitializeComponent();
    }

    /// <summary>Au lancement, ouvre la fenêtre principale.</summary>
    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _window = new MainWindow();
        _window.Activate();
    }
}
