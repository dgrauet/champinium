// Point d'entrée de l'application WinUI 3. Crée et active la fenêtre principale,
// enregistre le scheme `champinium://` et garantit l'instance unique.
// Présentation uniquement : aucune logique métier ici.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;
// Alias volontaire : ce namespace expose lui aussi un `LaunchActivatedEventArgs`,
// qui rendrait ambiguë la signature de `OnLaunched` (celui de Microsoft.UI.Xaml).
using WinRtActivation = Windows.ApplicationModel.Activation;

namespace Champinium;

public partial class App : Application
{
    private Window? _window;

    public App()
    {
        InitializeComponent();
    }

    /// <summary>
    /// Au lancement : enregistre le scheme, s'assure d'être la seule instance,
    /// ouvre la fenêtre principale, puis traite le lien qui a servi à lancer
    /// l'app (le cas échéant).
    /// </summary>
    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        RegisterUriScheme();

        var keyInstance = AppInstance.FindOrRegisterForKey("org.champinium.main");
        var activationArgs = AppInstance.GetCurrent().GetActivatedEventArgs();

        if (!keyInstance.IsCurrent)
        {
            // Instance secondaire : transmet l'activation à l'instance en cours
            // puis se termine — jamais deux nœuds sur le même dossier de données
            // (identité Ed25519, blockstore, .seed_index, seq du feed).
            _ = RedirectAndExitAsync(keyInstance, activationArgs);
            return;
        }

        // La fenêtre AVANT l'abonnement à `Activated` : `OnActivated` déréférence
        // `_window` (null-conditionnel) et laisserait tomber l'activation en
        // silence si elle arrivait d'abord. Or la construction de MainWindow
        // (InitializeComponent → parse XAML) est justement la partie lente du
        // démarrage, donc « lancer l'app puis cliquer un lien » tomberait dedans.
        _window = new MainWindow();
        keyInstance.Activated += OnActivated;
        _window.Activate();

        // Lien ayant servi à lancer l'app (démarrage à froid).
        HandleActivation(activationArgs, coldStart: true);
    }

    /// <summary>
    /// Enregistre le scheme `champinium://` pour l'utilisateur courant
    /// (HKCU — aucun droit admin). Rejoué à CHAQUE démarrage : idempotent, et
    /// le chemin de l'exe se corrige tout seul si le dossier portable est
    /// déplacé. Échec non fatal : un registre verrouillé (politique
    /// d'entreprise) ne doit pas empêcher d'utiliser l'app.
    /// </summary>
    private static void RegisterUriScheme()
    {
        try
        {
            var exe = Environment.ProcessPath;
            if (string.IsNullOrEmpty(exe))
            {
                return;
            }

            // 2e paramètre = logo : pour une app unpackaged, "<exe>,<index de ressource>".
            ActivationRegistrationManager.RegisterForProtocolActivation(
                "champinium", $"{exe},0", "Champinium", exe);
        }
        catch (Exception)
        {
            // non fatal — voir doc de la méthode
        }
    }

    /// <summary>
    /// Redirige l'activation vers l'instance déjà en cours, puis termine ce
    /// process. La redirection est attendue de façon ASYNCHRONE et non pas
    /// bloquante : `RedirectActivationToAsync` a besoin que le thread principal
    /// (STA) continue de pomper les messages COM, un `.Wait()` dessus depuis
    /// `OnLaunched` peut donc se bloquer. En rendant la main sans créer de
    /// fenêtre, la boucle de messages WinUI continue de tourner, la redirection
    /// aboutit, et la continuation tue le process.
    /// </summary>
    private static async Task RedirectAndExitAsync(AppInstance keyInstance, AppActivationArguments args)
    {
        try
        {
            await keyInstance.RedirectActivationToAsync(args);
        }
        catch (Exception)
        {
            // L'instance visée a pu disparaître entre-temps : on se termine
            // quand même, sans jamais ouvrir un 2e nœud sur le même dossier.
        }

        // `Exit()` est ignoré tant qu'aucune fenêtre n'a été créée : on termine
        // le process directement (pattern recommandé pour une instance redirigée).
        Process.GetCurrentProcess().Kill();
    }

    /// <summary>Activation reçue par l'instance en cours (app déjà ouverte).</summary>
    private void OnActivated(object? sender, AppActivationArguments args)
    {
        // Arrive sur un thread de travail : repasser sur le thread UI.
        _window?.DispatcherQueue.TryEnqueue(() => HandleActivation(args, coldStart: false));
    }

    /// <summary>
    /// Extrait un lien `champinium://` d'une activation et l'ouvre en aperçu.
    /// Trois sources, dans l'ordre : les args d'activation Protocol, la ligne de
    /// commande PORTÉE PAR CETTE activation, et — au seul démarrage à froid — la
    /// ligne de commande du process. Les deux replis sont nécessaires : en app
    /// UNPACKAGED, le cast des args Protocol échoue dans certaines versions
    /// (microsoft-ui-xaml#9225), et la forme enregistrée par
    /// <see cref="RegisterUriScheme"/> passe l'URI en argument (`"%1"`).
    /// <paramref name="coldStart"/> garde le dernier repli : la ligne de
    /// commande du PROCESS ne décrit que le lancement initial, la rejouer sur
    /// une activation ultérieure rouvrirait l'aperçu d'un lien périmé.
    /// Toutes les sources passent par <see cref="AsChannelLink"/> : rien d'autre
    /// qu'un `champinium://` n'atteint l'aperçu.
    /// </summary>
    private void HandleActivation(AppActivationArguments args, bool coldStart)
    {
        string? uri = null;

        if (args.Kind == ExtendedActivationKind.Protocol
            && args.Data is WinRtActivation.IProtocolActivatedEventArgs protocolArgs)
        {
            uri = AsChannelLink(protocolArgs.Uri?.AbsoluteUri);
        }

        if (uri is null)
        {
            // Repli du cast ci-dessus : en unpackaged, l'objet porté par `Data`
            // vient parfois d'une autre projection que l'interface attendue —
            // il expose quand même une propriété `Uri` (System.Uri des deux
            // côtés). On la lit sans dépendre du type exact.
            uri = AsChannelLink(
                (args.Data?.GetType().GetProperty("Uri")?.GetValue(args.Data) as Uri)?.AbsoluteUri);
        }

        if (uri is null && args.Data is WinRtActivation.ILaunchActivatedEventArgs launchArgs)
        {
            uri = FindLink(Tokenize(launchArgs.Arguments));
        }

        if (uri is null && coldStart)
        {
            uri = FindLink(Environment.GetCommandLineArgs().Skip(1));
        }

        if (uri is not null && _window is MainWindow main)
        {
            // Jamais d'abonnement automatique : ouvre l'APERÇU du channel.
            _ = main.OpenChannelLinkAsync(uri);
        }
    }

    /// <summary>Découpe une ligne de commande brute (`"exe" "uri"`) — une URI
    /// `champinium://&lt;peerid&gt;` ne contient jamais d'espace, un découpage
    /// naïf suffit donc pour la retrouver.</summary>
    private static string[] Tokenize(string? commandLine) =>
        (commandLine ?? "").Split(new[] { ' ', '\t' }, StringSplitOptions.RemoveEmptyEntries);

    private static string? FindLink(IEnumerable<string> tokens) =>
        tokens.Select(t => AsChannelLink(t.Trim('"'))).FirstOrDefault(t => t is not null);

    /// <summary>Filtre commun à TOUTES les sources d'activation (args Protocol,
    /// repli par réflexion, lignes de commande) : seul un lien `champinium://`
    /// est routé vers l'aperçu — une activation d'un autre scheme ou un argument
    /// quelconque est ignoré. La règle vit ici et nulle part ailleurs.</summary>
    private static string? AsChannelLink(string? candidate) =>
        candidate is not null
        && candidate.StartsWith("champinium://", StringComparison.OrdinalIgnoreCase)
            ? candidate
            : null;
}
