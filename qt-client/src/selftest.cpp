#include "selftest.h"

#include <QClipboard>
#include <QCoreApplication>
#include <QGuiApplication>
#include <QImage>
#include <QJsonDocument>
#include <QJsonObject>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QQuickItem>
#include <QQuickWindow>
#include <QTimer>

#include <cstdio>
#include <functional>

#include "ui/messagetext.h"

namespace decibell {

namespace {

void report(const QJsonObject &o)
{
    const QByteArray line = QJsonDocument(o).toJson(QJsonDocument::Compact) + '\n';
    if (const QByteArray out = qgetenv("DECIBELL_BENCH_OUT"); !out.isEmpty()) {
        if (FILE *f = fopen(out.constData(), "ab")) {
            fwrite(line.constData(), 1, size_t(line.size()), f);
            fclose(f);
        }
    }
    fputs(line.constData(), stdout);
    fflush(stdout);
}

void mouse(QQuickWindow *w, QEvent::Type type, QPointF scene, Qt::MouseButtons buttons)
{
    const Qt::MouseButton button = type == QEvent::MouseMove ? Qt::NoButton : Qt::LeftButton;
    QMouseEvent ev(type, scene, scene, w->mapToGlobal(scene), button, buttons, Qt::NoModifier);
    QCoreApplication::sendEvent(w, &ev);
}

QString selectedText(QObject *sel)
{
    QString text;
    QMetaObject::invokeMethod(sel, "selectedText", Q_RETURN_ARG(QString, text));
    return text;
}

void selectionTest(QQuickWindow *w, const QString &capture)
{
    auto *list = w->findChild<QQuickItem *>(QStringLiteral("messageList"));
    QObject *sel = w->findChild<QObject *>(QStringLiteral("chatSelection"));
    if (!list || !sel) {
        report({{QStringLiteral("test"), QStringLiteral("selection")}, {QStringLiteral("error"), QStringLiteral("no list")}});
        QCoreApplication::exit(1);
        return;
    }
    // A hidden window never renders, so nothing polishes it: the ListView hasn't placed its rows and the bodies were
    // laid out at width 0 (one character per line). A grab renders once, polish included.
    w->grabWindow();
    // A point inside row `index`'s message body (body coordinates), in the window.
    const auto body = [list](int index, qreal x, qreal y) -> QPointF {
        auto *content = list->property("contentItem").value<QQuickItem *>();
        for (QQuickItem *row : content->childItems()) {
            if (!row->isVisible() || row->property("index").toInt() != index)
                continue;
            std::function<MessageText *(QQuickItem *)> find = [&](QQuickItem *i) -> MessageText * {
                if (auto *mt = qobject_cast<MessageText *>(i); mt && mt->isVisible())
                    return mt;
                for (QQuickItem *c : i->childItems()) {
                    if (MessageText *mt = find(c))
                        return mt;
                }
                return nullptr;
            };
            if (MessageText *mt = find(row))
                return mt->mapToScene(QPointF(x, y));
        }
        return {};
    };
    QJsonObject o{{QStringLiteral("test"), QStringLiteral("selection")}};

    // 1. Drag from the start of the newest message's text up into the fourth newest.
    const QPointF from = body(0, 4, 10), to = body(3, 40, 10);
    o[QStringLiteral("points")] = QStringLiteral("%1,%2 -> %3,%4").arg(from.x()).arg(from.y()).arg(to.x()).arg(to.y());
    mouse(w, QEvent::MouseButtonPress, from, Qt::LeftButton);
    for (int i = 1; i <= 10; ++i)
        mouse(w, QEvent::MouseMove, from + (to - from) * (i / 10.0), Qt::LeftButton);
    mouse(w, QEvent::MouseButtonRelease, to, Qt::NoButton);
    const QString drag = selectedText(sel);
    // What each of the first rows' bodies was told to draw.
    {
        auto *content = list->property("contentItem").value<QQuickItem *>();
        QStringList rows;
        std::function<void(QQuickItem *, int)> walk = [&](QQuickItem *i, int index) {
            if (auto *mt = qobject_cast<MessageText *>(i); mt && mt->isVisible())
                rows << QStringLiteral("%1:[%2,%3)/%4").arg(index).arg(mt->property("selectionStart").toInt())
                            .arg(mt->property("selectionEnd").toInt()).arg(mt->length());
            for (QQuickItem *c : i->childItems())
                walk(c, index);
        };
        for (QQuickItem *row : content->childItems()) {
            const int index = row->property("index").toInt();
            if (row->isVisible() && index <= 4)
                walk(row, index);
        }
        o[QStringLiteral("row_ranges")] = rows.join(u' ');
    }
    o[QStringLiteral("drag_rows")] = int(drag.count(u'\n')) + 1;
    o[QStringLiteral("drag_text")] = drag.left(400);
    const bool dragOk = drag.count(u'\n') >= 3;

    // A capture with the selection showing.
    if (!capture.isEmpty()) {
        const QImage img = w->grabWindow();
        o[QStringLiteral("capture")] = img.save(capture) ? capture : QStringLiteral("FAILED");
    }

    // 2. A click collapses it.
    const QPointF click = body(0, 30, 10);
    mouse(w, QEvent::MouseButtonPress, click, Qt::LeftButton);
    mouse(w, QEvent::MouseButtonRelease, click, Qt::NoButton);
    const bool clickClears = selectedText(sel).isEmpty();
    o[QStringLiteral("click_clears")] = clickClears;

    // 3. A double click selects a word (the second word of the newest message).
    const QPointF word2 = body(0, 40, 10);
    mouse(w, QEvent::MouseButtonPress, word2, Qt::LeftButton);
    mouse(w, QEvent::MouseButtonRelease, word2, Qt::NoButton);
    mouse(w, QEvent::MouseButtonPress, word2, Qt::LeftButton);
    mouse(w, QEvent::MouseButtonDblClick, word2, Qt::LeftButton);
    mouse(w, QEvent::MouseButtonRelease, word2, Qt::NoButton);
    const QString word = selectedText(sel);
    o[QStringLiteral("word")] = word;
    const bool wordOk = !word.isEmpty() && !word.contains(u' ');

    // 4. Copy puts it on the clipboard.
    QMetaObject::invokeMethod(sel, "copy");
    const bool copyOk = QGuiApplication::clipboard()->text() == word;
    o[QStringLiteral("copy_ok")] = copyOk;

    const bool selectionOk = dragOk && clickClears && wordOk && copyOk;

    // 5. Keys scroll the focused list (the press above gave it focus): PageUp a page (Blink's step), then End
    // back to the newest.
    const qreal page = qMax(list->height() * 0.875, list->height() - 40);
    const qreal y0 = list->property("contentY").toReal();
    o[QStringLiteral("list_focused")] = list->hasActiveFocus();
    QKeyEvent up(QEvent::KeyPress, Qt::Key_PageUp, Qt::NoModifier);
    QCoreApplication::sendEvent(w, &up);
    QTimer::singleShot(400, w, [w, list, o, page, y0, selectionOk]() mutable {
        const qreal y1 = list->property("contentY").toReal();
        o[QStringLiteral("page_step")] = y0 - y1;
        const bool pageOk = qAbs((y0 - y1) - page) < 1;
        QKeyEvent end(QEvent::KeyPress, Qt::Key_End, Qt::NoModifier);
        QCoreApplication::sendEvent(w, &end);
        QTimer::singleShot(400, w, [list, o, y0, selectionOk, pageOk]() mutable {
            const bool endOk = qAbs(list->property("contentY").toReal() - y0) < 1;
            o[QStringLiteral("end_back")] = endOk;
            const bool pass = selectionOk && pageOk && endOk;
            o[QStringLiteral("pass")] = pass;
            report(o);
            QCoreApplication::exit(pass ? 0 : 1);
        });
    });
}

// History paging and new messages (run with --page-size N): what is on screen must not move.
void historyTest(QQuickWindow *w)
{
    auto *list = w->findChild<QQuickItem *>(QStringLiteral("messageList"));
    QObject *model = w->findChild<QObject *>(QStringLiteral("chatModel"));
    QJsonObject o{{QStringLiteral("test"), QStringLiteral("history")}};
    if (!list || !model || model->property("pageSize").toInt() <= 0) {
        o[QStringLiteral("error")] = QStringLiteral("needs the chat screen and --page-size N");
        report(o);
        QCoreApplication::exit(1);
        return;
    }
    auto settle = [w] {
        QCoreApplication::processEvents(); // Qt.callLater, queued updates
        w->grabWindow();                   // polish: the ListView places its rows
    };
    auto *content = list->property("contentItem").value<QQuickItem *>();
    // The delegate for model row `index` on screen: its top in list coordinates and its height.
    auto row = [&](int index, qreal *y, qreal *h) {
        for (QQuickItem *r : content->childItems()) {
            if (r->isVisible() && r->property("index").isValid() && r->property("index").toInt() == index) {
                *y = r->mapToItem(list, QPointF(0, 0)).y();
                *h = r->height();
                return true;
            }
        }
        return false;
    };
    // A row whose top is between 100 and 300 px down the list: the one being read.
    auto readingRow = [&]() {
        for (QQuickItem *r : content->childItems()) {
            const qreal y = r->mapToItem(list, QPointF(0, 0)).y();
            if (r->isVisible() && r->property("index").isValid() && y >= 100 && y <= 300)
                return r->property("index").toInt();
        }
        return -1;
    };
    auto maxY = [&] {
        return list->property("originY").toReal() + list->property("contentHeight").toReal() - list->height();
    };
    settle();
    o[QStringLiteral("initial_rows")] = model->property("count").toInt();
    bool pass = true;
    qreal y0 = 0, y1 = 0, h = 0;

    // A. Reading history 800 px up: the next older page lands above, the row being read stays put.
    list->setProperty("contentY", maxY() - 800);
    settle();
    int k = readingRow();
    int added = 0;
    if (k < 0 || !row(k, &y0, &h)) {
        o[QStringLiteral("error")] = QStringLiteral("no row on screen");
        pass = false;
    } else {
        QMetaObject::invokeMethod(model, "loadOlder", Q_RETURN_ARG(int, added));
        settle();
        row(k, &y1, &h);
        o[QStringLiteral("older_added")] = added;
        o[QStringLiteral("older_shift_px")] = y1 - y0;
        pass = pass && added > 0 && qAbs(y1 - y0) < 0.5;
    }

    // B. A new message arrives while reading history: the row being read stays put (its index moves up by one).
    k = readingRow();
    if (k >= 0 && row(k, &y0, &h)) {
        QMetaObject::invokeMethod(model, "receiveNew");
        settle();
        const bool found = row(k + 1, &y1, &h);
        o[QStringLiteral("new_while_reading_shift_px")] = found ? y1 - y0 : 99999.0;
        pass = pass && found && qAbs(y1 - y0) < 0.5;
    } else {
        pass = false;
    }

    // C. A new message arrives at the bottom of the conversation: it shows, its bottom on the list's.
    list->setProperty("contentY", maxY());
    settle();
    QMetaObject::invokeMethod(model, "receiveNew");
    settle();
    if (row(0, &y0, &h)) {
        o[QStringLiteral("new_at_bottom_gap_px")] = list->height() - (y0 + h);
        pass = pass && qAbs(list->height() - (y0 + h)) < 1;
    } else {
        o[QStringLiteral("new_at_bottom_gap_px")] = QStringLiteral("not on screen");
        pass = false;
    }

    // D. Scrolling near the top pages older history in by itself.
    const int before = model->property("count").toInt();
    list->setProperty("contentY", list->property("originY").toReal() + 200);
    settle();
    settle();
    o[QStringLiteral("auto_loaded")] = model->property("count").toInt() - before;
    pass = pass && model->property("count").toInt() > before;

    o[QStringLiteral("pass")] = pass;
    report(o);
    QCoreApplication::exit(pass ? 0 : 1);
}

} // namespace

void runSelfTest(QQuickWindow *window, const QString &name, const QString &capture)
{
    // Laid out without being shown (as --capture does): a never-created window gets no geometry on Windows.
    window->create();
    QTimer::singleShot(900, window, [window, name, capture] {
        if (name == u"selection") {
            selectionTest(window, capture);
        } else if (name == u"history") {
            historyTest(window);
        } else if (name == u"regrab") {
            // A hidden window renders each grab through a new QRhi: the scene graph is re-created every time, as
            // after a lost device. Three grabs with events processed between them, the last one saved: icons,
            // avatar gradients, status dots, emoji and code chips must all be there (src/ui/texturecache.h).
            window->grabWindow();
            QTimer::singleShot(300, window, [window, capture] {
                window->grabWindow();
                QTimer::singleShot(300, window, [window, capture] {
                    const QImage img = window->grabWindow();
                    const bool saved = !capture.isEmpty() && img.save(capture);
                    report({{QStringLiteral("test"), QStringLiteral("regrab")},
                            {QStringLiteral("capture"), saved ? capture : QStringLiteral("not saved")}});
                    QCoreApplication::exit(saved ? 0 : 1);
                });
            });
        } else if (name == u"hold") {
            // Memory measurement without a window on screen: render once (pictures decoded and uploaded, glyphs
            // cached), then stay up DECIBELL_HOLD_SECONDS (default 30) for VMMap / measure.ps1.
            window->grabWindow();
            QTimer::singleShot(1000, window, [window] { window->grabWindow(); }); // pictures that loaded async
            const int secs = qEnvironmentVariableIsSet("DECIBELL_HOLD_SECONDS")
                ? qEnvironmentVariableIntValue("DECIBELL_HOLD_SECONDS") : 30;
            report({{QStringLiteral("test"), name}, {QStringLiteral("pid"), qint64(QCoreApplication::applicationPid())}});
            QTimer::singleShot(secs * 1000, window, [] { QCoreApplication::exit(0); });
        } else {
            report({{QStringLiteral("test"), name}, {QStringLiteral("error"), QStringLiteral("unknown test")}});
            QCoreApplication::exit(1);
        }
    });
}

} // namespace decibell
