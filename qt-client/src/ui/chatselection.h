// Text selection across the rows of a message list, as the renderer has it in Chromium (MessageBubble's
// `select-text` containers): press and drag over message bodies, the selection runs through every row between, the
// list scrolls while the pointer is held past its top or bottom edge, a double click selects a word, Ctrl+C copies.
// A click on a link opens it; the pointer is a hand over links and an I-beam over text.
//
// The selection lives here, not in the rows: delegates come and go while scrolling (and are reused), so each row's
// MessageText asks for its own range (startFor / endFor, re-read when `version` changes). Positions are text
// positions inside a row's MessageText. Rows are model indexes of a newest-first model, so an earlier message has a
// larger index. (The client will key rows by message id, so new messages arriving don't move a selection.)
#pragma once

#include <QAbstractItemModel>
#include <QElapsedTimer>
#include <QObject>
#include <QPointF>
#include <QPointer>
#include <QQuickItem>
#include <QTimer>
#include <QtQml/qqmlregistration.h>

namespace decibell {

class MessageText;

class ChatSelection : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    // The ListView of message rows. Each delegate has an `index` and (at most) one MessageText body.
    Q_PROPERTY(QQuickItem *target READ target WRITE setTarget NOTIFY targetChanged)
    // The rows' model, for copying rows that have no delegate: roles "content", "senderName", "timeLabel", "grouped".
    Q_PROPERTY(QAbstractItemModel *model MEMBER m_model NOTIFY targetChanged)
    // Bumped on every change; rows bind their range to it.
    Q_PROPERTY(int version READ version NOTIFY changed)
    Q_PROPERTY(bool hasSelection READ hasSelection NOTIFY changed)

public:
    explicit ChatSelection(QObject *parent = nullptr);

    QQuickItem *target() const { return m_target; }
    void setTarget(QQuickItem *t);
    int version() const { return m_version; }
    bool hasSelection() const;

    // A row's selected range in its body text (length: the body's length), or -1 when the row isn't selected.
    Q_INVOKABLE int startFor(int row) const;
    Q_INVOKABLE int endFor(int row, int length) const;
    Q_INVOKABLE void clear();
    // Puts the selection on the clipboard as text: bodies as displayed, a sender line ("name time") before each
    // message that starts a group after the first, rows separated by line breaks.
    Q_INVOKABLE void copy() const;
    Q_INVOKABLE QString selectedText() const;

signals:
    void targetChanged();
    void changed();
    void linkActivated(const QString &url);

protected:
    bool eventFilter(QObject *watched, QEvent *event) override;

private:
    struct Pos {
        int row = -1;
        int pos = 0;
    };
    struct Hit {
        Pos at;
        MessageText *body = nullptr;
        QPointF local;  // the point in the body's coordinates
        bool valid = false;
    };
    Hit hitAt(QPointF listPoint) const;
    static bool before(const Pos &a, const Pos &b) { return a.row > b.row || (a.row == b.row && a.pos < b.pos); }
    void setFocus(const Pos &p);
    void autoScroll();
    void updateCursor(QPointF listPoint);
    void bump();

    QPointer<QQuickItem> m_target;
    QAbstractItemModel *m_model = nullptr;
    int m_version = 0;
    Pos m_anchor, m_focus;
    bool m_active = false;
    bool m_dragging = false;
    bool m_moved = false;
    QPointF m_pressPoint, m_lastPoint;
    QString m_pressLink;
    QTimer m_scrollTimer;
    QElapsedTimer m_scrollClock;
};

} // namespace decibell
