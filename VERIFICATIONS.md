# Vérifications locales — 24 octobre 2026

Version évaluée : **1.1.0**, Windows x64 MSVC, Rust **1.99.1**.

- Formatage : conforme.
- Tests : **53 réussis** dans une exécution avec 32 threads, moteur OCR portable fourni et test complémentaire explicitement activé. Sans ce moteur, la commande ordinaire exécute 52 tests et ignore le test OCR complémentaire.
- Test OCR complémentaire : réussi avec le Tesseract portable fourni sur le titre synthétique aux échelles 80 %, 100 %, 125 % et 150 %.
- Clippy : aucun avertissement avec `-D warnings`.
- Compilation optimisée Windows : réussie.
- Scénarios automatisés : écritures concurrentes, suppression suivie de résultats tardifs, journal de préparation interrompu avant et après validation SQLite, sélection des images, refus d'écrasement au renommage, répartition des lots, persistance du statut des lots, limite de temps OCR et protection des captures actives lors du nettoyage.
- Parcours Windows sur données fictives : ouverture, galerie et miniatures, renommage, sélection multiple, préparation de cinq images en dossiers de 2, 2 et 1, rafraîchissement de la galerie, statut Envoyé, profil enregistré et appliqué, mesure du stockage et aperçu du diagnostic.
- Instance unique : un lancement supplémentaire se termine et conserve une seule fenêtre de test.
- Notices : chaque dépendance Windows active possède une notice correspondant à sa version.

## Avis de sécurité

Contrôle avec **cargo-audit 0.22.2**, base RustSec consultée le 24 octobre 2026 : **aucune vulnérabilité signalée parmi les dépendances Windows actives**.

Le verrou multiplateforme contient quatre signalements quick-xml correspondant aux cibles non Windows. Ces composants n'apparaissent pas dans l'arbre de dépendances Windows de cette application. Trois composants Windows sont signalés non maintenus : **fxhash 0.2.1**, **paste 1.0.15** et **ttf-parser 0.25.1**. Cela reste un point de maintenance ; ces avis ne sont pas supprimés du contrôle.

Ce contrôle ne constitue pas un audit exhaustif du code et ne couvre pas les bibliothèques C/C++ de l'OCR. Le moteur fourni conserve ses versions et notices identiques au paquet portable déjà utilisé.

## Essais restant à faire en usage réel

Les essais d'interface ci-dessus utilisent des images fictives. Vérifie encore la reconnaissance sur ton parcours de jeu, les infobulles, les différentes échelles d'affichage, les changements d'écran, le raccourci en arrière-plan et la fermeture pendant une longue file réelle.

Les durées de la nouvelle interface séparent attente, OCR et sauvegarde. Elles ne doivent pas être comparées directement à l'ancien indicateur regroupé. Aucun gain chiffré sous 0,4 seconde n'est garanti.

## Organisation et compatibilité

Les états des fiches, de l’OCR, de l’analyse et du filtre de galerie sont typés. Les erreurs distinguent système de fichiers, SQLite, JSON, image, règles de validation, délai OCR et échec du processus. Les causes d’origine sont conservées jusqu’à l’affichage ou au journal ; aucun traitement ne classe une erreur en analysant son texte français.

Le code des événements, de la galerie, des écritures du suivi, des dialogues, du tableau de bord et des paramètres est réparti en modules. Les valeurs historiques SQLite restent compatibles, y compris une valeur inconnue conservée sans conversion silencieuse. Les tests couvrent cette compatibilité et la conservation des causes d’erreur.

Les parcours d’interface de la version alternative ont été effectués sur des données fictives. La version 1.1.0 compilée a également été vérifiée sur un profil isolé : lecture d’un historique au format 1.0, galerie et aperçu, reconnaissance du moteur intégré, préparation de cinq images fictives en dossiers de 2, 2 et 1, retrait des PNG du dossier actif et enregistrement du statut Envoyé. Les captures de documentation sont issues de cette instance et recadrées pour exclure la barre de titre et le curseur. Les essais en jeu restent à effectuer par l’utilisateur.
