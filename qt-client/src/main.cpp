// Decibell Qt client entry point.
//
//   decibell-qt [--theme <name>] [--capture <file.png> [--capture-delay <ms>]]
//   decibell-qt --video a.dbv[,b.dbv] [--bench-video <s>] [--maximized]   (spike (a): watched streams)
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

#include "crash_trace.h"
#include "event_trace.h"
#include "frame_stats.h"
#include "gpu_device.h"
#include "selftest.h"
#include "ui/emoji.h"
#include "video/videoitem.h"

Q_IMPORT_QML_PLUGIN(Decibell_ThemePlugin)
Q_IMPORT_QML_PLUGIN(Decibell_AppPlugin)
Q_IMPORT_QML_PLUGIN(Decibell_UiPlugin)
Q_IMPORT_QML_PLUGIN(Decibell_VideoPlugin)

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
    decibell::installCrashTrace();
    // Glyph-cache text through the platform engine (DirectWrite on Windows): the closest to Chromium's text.
    QQuickWindow::setTextRenderType(QQuickWindow::NativeTextRendering);
    // Frame pacing without idle wakeups (both measured on a 280 Hz panel, qt-client/HANDOFF.md §2b):
    // - Qt's DXGI vblank thread waits on every vertical blank whether or not anything renders (279 wakeups a
    //   second here) and delivers window update requests on it. Off.
    // - Without it, a window's update request falls back to a 5 ms timer, which Windows' 15.6 ms timer granularity
    //   turns into ~65 frames a second. QT_QPA_UPDATE_IDLE_TIME=0 delivers the request at once instead, and the
    //   frame is paced by the swap chain (Qt waits on its frame-latency object before each frame), so animation
    //   runs at the refresh rate and nothing wakes while nothing changes.
    // Scrolling at 1000 px/s: 280 frames/s with 3-8 missed vsyncs in 6 s (Electron: 45-90); idle: our threads ~0.
    if (!qEnvironmentVariableIsSet("QT_D3D_NO_VBLANK_THREAD"))
        qputenv("QT_D3D_NO_VBLANK_THREAD", "1");
    if (!qEnvironmentVariableIsSet("QT_QPA_UPDATE_IDLE_TIME"))
        qputenv("QT_QPA_UPDATE_IDLE_TIME", "0");

    QGuiApplication app(argc, argv);
    QGuiApplication::setApplicationName(QStringLiteral("Decibell"));

    QCommandLineParser cli;
    const QCommandLineOption themeOpt(QStringLiteral("theme"), QStringLiteral("Theme name."), QStringLiteral("name"));
    const QCommandLineOption captureOpt(QStringLiteral("capture"), QStringLiteral("Save the first screen and quit."), QStringLiteral("file"));
    const QCommandLineOption delayOpt(QStringLiteral("capture-delay"), QStringLiteral("Delay before capturing."), QStringLiteral("ms"), QStringLiteral("400"));
    const QCommandLineOption screenOpt(QStringLiteral("screen"), QStringLiteral("chat | gallery."), QStringLiteral("name"), QStringLiteral("chat"));
    const QCommandLineOption fixtureOpt(QStringLiteral("fixture"), QStringLiteral("Fixture folder (tools/gen-fixture.mjs)."),
                                        QStringLiteral("dir"), QStringLiteral(DECIBELL_FIXTURE_DIR));
    const QCommandLineOption benchOpt(QStringLiteral("bench-scroll"),
                                      QStringLiteral("Scroll the chat at a constant speed for N seconds, print frame stats (JSON) and quit."),
                                      QStringLiteral("seconds"));
    const QCommandLineOption speedOpt(QStringLiteral("bench-speed"), QStringLiteral("Scroll speed in px/s."), QStringLiteral("px"),
                                      QStringLiteral("3000"));
    const QCommandLineOption ablateOpt(QStringLiteral("ablate"), QStringLiteral("Debug: hide parts of message rows (avatar,header,body,images,divider)."),
                                       QStringLiteral("parts"));
    const QCommandLineOption videoOpt(QStringLiteral("video"),
                                      QStringLiteral("Watch test streams (.dbv, tools/streamgen), comma-separated."),
                                      QStringLiteral("files"));
    const QCommandLineOption benchVideoOpt(
        QStringLiteral("bench-video"),
        QStringLiteral("With --video: record N seconds, print frame + stream stats (JSON) and quit."),
        QStringLiteral("seconds"));
    const QCommandLineOption maximizedOpt(QStringLiteral("maximized"), QStringLiteral("Show the window maximized."));
    const QCommandLineOption selfTestOpt(QStringLiteral("selftest"),
                                         QStringLiteral("Run a self-test in the hidden window (selection); --capture "
                                                        "saves the window mid-test."),
                                         QStringLiteral("name"));
    cli.addOptions({themeOpt, captureOpt, delayOpt, screenOpt, fixtureOpt, benchOpt, speedOpt, ablateOpt, videoOpt,
                    benchVideoOpt, maximizedOpt, selfTestOpt});
    cli.addHelpOption();
    cli.process(app);

    decibell::installEventTrace();
    registerFonts();
    decibell::Emoji::instance().setDirectory(QStringLiteral(DECIBELL_ASSETS_DIR "/twemoji"));

    QQmlApplicationEngine engine;
    QVariantMap initial;
    if (cli.isSet(themeOpt))
        initial.insert(QStringLiteral("themeName"), cli.value(themeOpt));
    initial.insert(QStringLiteral("screenName"), cli.isSet(videoOpt) ? QStringLiteral("video") : cli.value(screenOpt));
    if (cli.isSet(videoOpt))
        initial.insert(QStringLiteral("videoSources"), cli.value(videoOpt).split(QLatin1Char(','), Qt::SkipEmptyParts));
    initial.insert(QStringLiteral("benchVideo"), cli.isSet(benchVideoOpt));
    initial.insert(QStringLiteral("fixtureDir"), cli.value(fixtureOpt));
    initial.insert(QStringLiteral("ablate"), cli.value(ablateOpt));
    if (cli.isSet(benchOpt))
        initial.insert(QStringLiteral("benchSpeed"), cli.value(speedOpt).toDouble());
    // The window starts hidden so our graphics device is in place before Qt creates one (gpu_device.h). A capture
    // never shows it: grabWindow() renders the unexposed window through the same RHI backend and platform font
    // engine (D3D11 + DirectWrite), so nothing flashes on screen; QT_QPA_PLATFORM=offscreen would swap in FreeType.
    initial.insert(QStringLiteral("visible"), false);
    engine.setInitialProperties(initial);
    engine.loadFromModule("Decibell.App", "Main");
    if (engine.rootObjects().isEmpty())
        return 1;
    auto *mainWindow = qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
    decibell::installGraphicsDevice(mainWindow);
    if (cli.isSet(selfTestOpt) && mainWindow) {
        decibell::runSelfTest(mainWindow, cli.value(selfTestOpt), cli.value(captureOpt));
        return app.exec();
    }
    if (mainWindow && !cli.isSet(captureOpt)) {
        if (cli.isSet(maximizedOpt))
            mainWindow->showMaximized();
        else
            mainWindow->show();
    }

    const bool benchVideo = cli.isSet(benchVideoOpt);
    if ((cli.isSet(benchOpt) || benchVideo) && mainWindow) {
        // 2 s for the fixture to load and the first screen to settle, then N s of recording while QML scrolls.
        auto *stats = new decibell::FrameStats(mainWindow);
        const int seconds = cli.value(benchVideo ? benchVideoOpt : benchOpt).toInt();
        QTimer::singleShot(2000, &app, [stats, benchVideo] {
            stats->start();
            if (benchVideo)
                decibell::video::VideoItem::benchStart();
        });
        QTimer::singleShot(2000 + seconds * 1000, &app, [stats, benchVideo] {
            const QString report = benchVideo ? QStringLiteral("{\"frames\":%1,\"video\":%2}")
                                                    .arg(stats->report(), decibell::video::VideoItem::benchReport())
                                              : stats->report();
            // A WIN32 app has no console: write the report next to nothing else, to a file when asked for.
            const QByteArray line = report.toUtf8() + '\n';
            if (const QByteArray out = qgetenv("DECIBELL_BENCH_OUT"); !out.isEmpty()) {
                if (FILE *f = fopen(out.constData(), "ab")) {
                    fwrite(line.constData(), 1, size_t(line.size()), f);
                    fclose(f);
                }
            }
            fputs(line.constData(), stdout);
            fflush(stdout);
            QCoreApplication::exit(0);
        });
    }

    if (cli.isSet(captureOpt)) {
        auto *window = qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
        // Create the native window without showing it: a never-created window gets no geometry on Windows, so
        // its content item would lay out at width 0.
        if (window)
            window->create();
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
