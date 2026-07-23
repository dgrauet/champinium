// Convertisseurs XAML minimaux — présentation uniquement, aucune logique métier.
using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace Champinium;

/// <summary>Chaîne vide → Visible, chaîne non vide → Collapsed (utilisé pour
/// masquer les catalogues normaux pendant une recherche active).</summary>
public sealed class EmptyStringToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        string.IsNullOrWhiteSpace(value as string) ? Visibility.Visible : Visibility.Collapsed;

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotImplementedException();
}

/// <summary>Inverse du précédent — affiche les résultats de recherche uniquement
/// quand la requête n'est pas vide.</summary>
public sealed class NonEmptyStringToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        string.IsNullOrWhiteSpace(value as string) ? Visibility.Collapsed : Visibility.Visible;

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotImplementedException();
}

/// <summary>Booléen → Visibility (utilisé pour masquer le bouton de pin sur les
/// lignes d'Explorer — le gabarit d'item est partagé avec Abonnements).</summary>
public sealed class BooleanToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        value is true ? Visibility.Visible : Visibility.Collapsed;

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotImplementedException();
}

/// <summary>Inverse d'un booléen (utilisé pour désactiver le bouton "Aperçu"
/// pendant le chargement — voir <c>NodeViewModel.IsPreviewLoading</c>).</summary>
public sealed class InverseBooleanConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        !(value is true);

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotImplementedException();
}
