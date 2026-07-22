// Réglages locaux front-only (pas de logique métier — présentation uniquement).
// `ApplicationData.Current` lève une exception pour une app "unpackaged"
// (WindowsPackageType=None dans Champinium.csproj), donc on ne peut pas s'en
// servir ici. On utilise un fichier minimal sous %LocalAppData%\Champinium\,
// séparé du répertoire de données du nœud (`DefaultDataDir()` côté core) pour
// ne pas mélanger état front et état protocole.
using System;
using System.IO;

namespace Champinium;

internal static class LocalSettings
{
    private static string SettingsDir =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Champinium");

    private static string ExplorerAcceptedFlagPath => Path.Combine(SettingsDir, "explorer_accepted.flag");

    /// <summary>Vrai si l'avertissement Explorer a déjà été accepté lors d'une session précédente.</summary>
    public static bool ExplorerAccepted
    {
        get
        {
            try
            {
                return File.Exists(ExplorerAcceptedFlagPath);
            }
            catch (IOException)
            {
                return false;
            }
            catch (UnauthorizedAccessException)
            {
                return false;
            }
        }
        set
        {
            try
            {
                Directory.CreateDirectory(SettingsDir);
                if (value)
                {
                    File.WriteAllText(ExplorerAcceptedFlagPath, "1");
                }
                else if (File.Exists(ExplorerAcceptedFlagPath))
                {
                    File.Delete(ExplorerAcceptedFlagPath);
                }
            }
            catch (IOException) { }
            catch (UnauthorizedAccessException) { }
        }
    }
}
