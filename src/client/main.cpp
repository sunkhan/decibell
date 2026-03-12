#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QDebug>
#include <QQuickStyle>
#include <QWindow>
#include "backend.hpp"

#ifdef _WIN32
#include <dwmapi.h>
#ifndef DWMWA_USE_IMMERSIVE_DARK_MODE
#define DWMWA_USE_IMMERSIVE_DARK_MODE 20
#endif
#ifndef DWMWA_BORDER_COLOR
#define DWMWA_BORDER_COLOR 34
#endif
#ifndef DWMWA_CAPTION_COLOR
#define DWMWA_CAPTION_COLOR 35
#endif
#endif

int main(int argc, char *argv[]) {
    QGuiApplication app(argc, argv);

    // Apply the qtquickcontrols2.conf styling
    QQuickStyle::setStyle("Basic");

    ChatBackend backend;

    QQmlApplicationEngine engine;

    // Bind the C++ backend to the QML context
    engine.rootContext()->setContextProperty("backend", &backend);

    // Load the root QML file using the custom URI from CMakeLists.txt
    const QUrl url(QStringLiteral("qrc:/ChatProj/src/client/App.qml"));

    QObject::connect(&engine, &QQmlApplicationEngine::objectCreated,
                     &app, [url](QObject *obj, const QUrl &objUrl) {
        if (!obj && url == objUrl)
            QCoreApplication::exit(-1);

#ifdef _WIN32
        if (obj) {
            QWindow* window = qobject_cast<QWindow*>(obj);
            if (window) {
                HWND hwnd = reinterpret_cast<HWND>(window->winId());
                // Dark mode for light-colored window button icons
                BOOL darkMode = TRUE;
                DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &darkMode, sizeof(darkMode));
                // Near-black title bar and border
                COLORREF captionColor = RGB(12, 13, 15); // #0C0D0F
                DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &captionColor, sizeof(captionColor));
                COLORREF borderColor = RGB(12, 13, 15);
                DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &borderColor, sizeof(borderColor));
            }
        }
#endif
    }, Qt::QueuedConnection);

    engine.load(url);

    return app.exec();
}