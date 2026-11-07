// Decibell Qt client entry point.
//
//   decibell-qt [--theme <name>] [--capture <file.png> [--capture-delay <ms>]]
//
// --capture renders the first screen, saves it and quits: the capture harness runs it with
// QT_QPA_PLATFORM=offscreen and compares against the Electron harness's screenshots.
#include <QCommandLineParser>
#include <QFontDatabase>
#include <QGuiApplication>
#include <QImage>
#include <QQmlApplicationEngine>
#include <QQuickWindow>
#include <QResource>
#include <QTimer>
#include <QtQml/qqmlextensionplugin.h>

Q_IMPORT_QML_PLUGIN(Decibell_ThemePlugin)
Q_IMPORT_QML_PLUGIN(Decibell_AppPlugin)

namespace {

// The same three variable fonts the renderer uses (see tools/fetch-assets.mjs). Registered from the
// uncompressed resource in the mapped executable, so Qt reads them in place instead of copying ~1.4 MB.
void registerFonts()
{
    for (const char *path : {":/fonts/MonaSans.ttf", ":/fonts/Inter.ttf", ":/fonts/JetBrainsMono.ttf"}) {
        const QResource res(QString::fromLatin1(path));
        if (!res.isValid() || res.compressionAlgorithm() != QResource::NoCompression) {
            qWarning("font resource %s missing or compressed", path);
            continue;
        }
        const auto data = QByteArray::fromRawData(reinterpret_cast<const char *>(res.data()), qsizetype(res.size()));
        if (QFontDatabase::addApplicationFontFromData(data) < 0)
            qWarning("font %s failed to register", path);
    }
}

} // namespace

int main(int argc, char *argv[])
{
    // Glyph-cache text through the platform engine (DirectWrite on Windows): the closest to Chromium's text.
    QQuickWindow::setTextRenderType(QQuickWindow::NativeTextRendering);

    QGuiApplication app(argc, argv);
    QGuiApplication::setApplicationName(QStringLiteral("Decibell"));

    QCommandLineParser cli;
    const QCommandLineOption themeOpt(QStringLiteral("theme"), QStringLiteral("Theme name."), QStringLiteral("name"));
    const QCommandLineOption captureOpt(QStringLiteral("capture"), QStringLiteral("Save the first screen and quit."), QStringLiteral("file"));
    const QCommandLineOption delayOpt(QStringLiteral("capture-delay"), QStringLiteral("Delay before capturing."), QStringLiteral("ms"), QStringLiteral("400"));
    cli.addOptions({themeOpt, captureOpt, delayOpt});
    cli.addHelpOption();
    cli.process(app);

    registerFonts();

    QQmlApplicationEngine engine;
    QVariantMap initial;
    if (cli.isSet(themeOpt))
        initial.insert(QStringLiteral("themeName"), cli.value(themeOpt));
    // A capture never shows the window: grabWindow() renders an unexposed window through the same RHI backend
    // and platform font engine (D3D11 + DirectWrite on Windows), so the image matches the screen without
    // flashing a window. QT_QPA_PLATFORM=offscreen would swap in FreeType and a different rasteriser.
    if (cli.isSet(captureOpt))
        initial.insert(QStringLiteral("visible"), false);
    engine.setInitialProperties(initial);
    engine.loadFromModule("Decibell.App", "Main");
    if (engine.rootObjects().isEmpty())
        return 1;

    if (cli.isSet(captureOpt)) {
        auto *window = qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
        const QString file = cli.value(captureOpt);
        QTimer::singleShot(cli.value(delayOpt).toInt(), &app, [window, file] {
            const QImage img = window ? window->grabWindow() : QImage();
            const bool ok = !img.isNull() && img.save(file);
            qInfo("capture %s: %s (%dx%d)", ok ? "saved" : "FAILED", qPrintable(file), img.width(), img.height());
            QCoreApplication::exit(ok ? 0 : 2);
        });
    }
    return app.exec();
}
