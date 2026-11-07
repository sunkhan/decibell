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

} // namespace

void runSelfTest(QQuickWindow *window, const QString &name, const QString &capture)
{
    // Laid out without being shown (as --capture does): a never-created window gets no geometry on Windows.
    window->create();
    QTimer::singleShot(900, window, [window, name, capture] {
        if (name == u"selection") {
            selectionTest(window, capture);
        } else {
            report({{QStringLiteral("test"), name}, {QStringLiteral("error"), QStringLiteral("unknown test")}});
            QCoreApplication::exit(1);
        }
    });
}

} // namespace decibell
