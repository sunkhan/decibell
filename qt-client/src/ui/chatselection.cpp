#include "chatselection.h"

#include "messagetext.h"

#include <QClipboard>
#include <QCursor>
#include <QGuiApplication>
#include <QHoverEvent>
#include <QMouseEvent>
#include <QStyleHints>
#include <QtMath>

#include <climits>
#include <functional>

namespace decibell {

namespace {

constexpr int kScrollTickMs = 16;

// The first visible MessageText in a delegate's subtree (a row has at most one body).
MessageText *findBody(QQuickItem *item)
{
    if (auto *mt = qobject_cast<MessageText *>(item))
        return mt->isVisible() ? mt : nullptr;
    for (QQuickItem *child : item->childItems()) {
        if (MessageText *mt = findBody(child))
            return mt;
    }
    return nullptr;
}

int roleOf(const QAbstractItemModel *model, const char *name)
{
    const auto roles = model->roleNames();
    for (auto it = roles.cbegin(); it != roles.cend(); ++it) {
        if (it.value() == name)
            return it.key();
    }
    return -1;
}

} // namespace

ChatSelection::ChatSelection(QObject *parent) : QObject(parent)
{
    m_scrollTimer.setInterval(kScrollTickMs);
    connect(&m_scrollTimer, &QTimer::timeout, this, &ChatSelection::autoScroll);
}

void ChatSelection::setTarget(QQuickItem *t)
{
    if (m_target == t)
        return;
    if (m_target)
        m_target->removeEventFilter(this);
    m_target = t;
    if (m_target) {
        m_target->installEventFilter(this);
        m_target->setAcceptHoverEvents(true);
    }
    emit targetChanged();
}

bool ChatSelection::hasSelection() const
{
    return m_active && (m_anchor.row != m_focus.row || m_anchor.pos != m_focus.pos);
}

void ChatSelection::bump()
{
    ++m_version;
    emit changed();
}

void ChatSelection::clear()
{
    if (!m_active)
        return;
    m_active = false;
    bump();
}

int ChatSelection::startFor(int row) const
{
    if (!hasSelection())
        return -1;
    const Pos &s = before(m_anchor, m_focus) ? m_anchor : m_focus;
    const Pos &e = before(m_anchor, m_focus) ? m_focus : m_anchor;
    if (row > s.row || row < e.row)
        return -1;
    return row == s.row ? s.pos : 0;
}

int ChatSelection::endFor(int row, int length) const
{
    if (!hasSelection())
        return -1;
    const Pos &s = before(m_anchor, m_focus) ? m_anchor : m_focus;
    const Pos &e = before(m_anchor, m_focus) ? m_focus : m_anchor;
    if (row > s.row || row < e.row)
        return -1;
    return row == e.row ? e.pos : length;
}

QString ChatSelection::selectedText() const
{
    if (!hasSelection() || !m_model)
        return {};
    const Pos &s = before(m_anchor, m_focus) ? m_anchor : m_focus;
    const Pos &e = before(m_anchor, m_focus) ? m_focus : m_anchor;
    const int contentRole = roleOf(m_model, "content");
    const int nameRole = roleOf(m_model, "senderName");
    const int timeRole = roleOf(m_model, "timeLabel");
    const int groupedRole = roleOf(m_model, "grouped");
    QStringList parts;
    for (int row = s.row; row >= e.row; --row) {
        const QModelIndex idx = m_model->index(row, 0);
        QString part;
        if (row != s.row && groupedRole >= 0 && !m_model->data(idx, groupedRole).toBool())
            part = m_model->data(idx, nameRole).toString() + u' ' + m_model->data(idx, timeRole).toString() + u'\n';
        const QString content = m_model->data(idx, contentRole).toString();
        part += MessageText::plainTextOf(content, row == s.row ? s.pos : 0, row == e.row ? e.pos : INT_MAX);
        // A selection that starts at the very end of a message (or ends at the very start of one) takes nothing
        // from it, and no line break for it either.
        if (part.isEmpty() && (row == s.row || row == e.row))
            continue;
        parts.append(part);
    }
    return parts.join(u'\n');
}

void ChatSelection::copy() const
{
    const QString text = selectedText();
    if (!text.isEmpty())
        QGuiApplication::clipboard()->setText(text);
}

ChatSelection::Hit ChatSelection::hitAt(QPointF p) const
{
    Hit hit;
    if (!m_target)
        return hit;
    // Rows tile the list without gaps; a point past an edge is read on the edge row.
    const qreal y = qBound<qreal>(1, p.y(), m_target->height() - 1);
    // The delegate under the point, from the delegates' own geometry. Not ListView.itemAt: with
    // verticalLayoutDirection BottomToTop it answered row 0 for every point. Pooled delegates (reuseItems) are
    // hidden and skipped.
    QQuickItem *row = nullptr;
    if (auto *content = m_target->property("contentItem").value<QQuickItem *>()) {
        for (QQuickItem *child : content->childItems()) {
            if (!child->isVisible() || !child->property("index").isValid())
                continue;
            const QPointF local = child->mapFromItem(m_target, QPointF(p.x(), y));
            if (local.y() >= 0 && local.y() < child->height()) {
                row = child;
                break;
            }
        }
    }
    if (!row)
        return hit;
    hit.valid = true;
    hit.at.row = row->property("index").toInt();
    hit.body = findBody(row);
    if (!hit.body)
        return hit; // no text in this row (an image-only message): its position 0
    hit.local = hit.body->mapFromItem(m_target, QPointF(p.x(), y));
    if (p.y() < 0 || hit.local.y() < 0)
        hit.at.pos = 0;
    else if (p.y() >= m_target->height() || hit.local.y() >= hit.body->height())
        hit.at.pos = hit.body->length();
    else
        hit.at.pos = hit.body->positionAt(hit.local.x(), hit.local.y());
    return hit;
}

void ChatSelection::setFocus(const Pos &p)
{
    if (p.row == m_focus.row && p.pos == m_focus.pos && m_active)
        return;
    m_focus = p;
    m_active = true;
    bump();
}

void ChatSelection::autoScroll()
{
    if (!m_dragging || !m_target) {
        m_scrollTimer.stop();
        return;
    }
    const qreal h = m_target->height();
    const qreal over = m_lastPoint.y() < 0 ? m_lastPoint.y() : m_lastPoint.y() > h ? m_lastPoint.y() - h : 0;
    if (over == 0) {
        m_scrollTimer.stop();
        return;
    }
    const qreal dt = qMin<qreal>(0.05, m_scrollClock.restart() / 1000.0);
    // Speed grows with the distance past the edge, as in Chromium's autoscroll.
    const qreal speed = qBound<qreal>(80, qAbs(over) * 12, 4000) * (over < 0 ? -1 : 1);
    const qreal origin = m_target->property("originY").toReal();
    const qreal content = m_target->property("contentHeight").toReal();
    const qreal y = qBound(origin, m_target->property("contentY").toReal() + speed * dt, qMax(origin, origin + content - h));
    m_target->setProperty("contentY", y);
    const Hit hit = hitAt(m_lastPoint);
    if (hit.valid)
        setFocus(hit.at);
}

void ChatSelection::updateCursor(QPointF p)
{
    Qt::CursorShape shape = Qt::ArrowCursor;
    const Hit hit = hitAt(p);
    if (hit.body && hit.local.y() >= 0 && hit.local.y() < hit.body->height()) {
        if (!hit.body->linkAt(hit.local.x(), hit.local.y()).isEmpty())
            shape = Qt::PointingHandCursor;
        else if (hit.body->isOverText(hit.local.x(), hit.local.y()))
            shape = Qt::IBeamCursor;
    }
    if (m_target->cursor().shape() != shape)
        m_target->setCursor(shape);
}

bool ChatSelection::eventFilter(QObject *watched, QEvent *event)
{
    if (watched != m_target)
        return QObject::eventFilter(watched, event);
    static const QByteArray trace = qgetenv("DECIBELL_SEL_TRACE");
    if (!trace.isEmpty() && (event->type() == QEvent::MouseButtonPress || event->type() == QEvent::MouseMove
                             || event->type() == QEvent::MouseButtonRelease || event->type() == QEvent::MouseButtonDblClick)) {
        const QPointF p = static_cast<QMouseEvent *>(event)->position();
        const Hit hit = hitAt(p);
        if (FILE *f = fopen(trace.constData(), "ab")) {
            fprintf(f, "ev %d at %.0f,%.0f valid %d row %d pos %d body %d local %.0f,%.0f\n", int(event->type()), p.x(),
                    p.y(), hit.valid, hit.at.row, hit.at.pos, hit.body != nullptr, hit.local.x(), hit.local.y());
            fclose(f);
        }
    }
    switch (event->type()) {
    case QEvent::MouseButtonPress: {
        auto *me = static_cast<QMouseEvent *>(event);
        if (me->button() != Qt::LeftButton)
            return false;
        const QPointF p = me->position();
        const Hit hit = hitAt(p);
        m_pressPoint = m_lastPoint = p;
        m_moved = false;
        m_dragging = true;
        m_pressLink = hit.body ? hit.body->linkAt(hit.local.x(), hit.local.y()) : QString();
        m_target->forceActiveFocus(Qt::MouseFocusReason);
        if (hit.valid) {
            if (me->modifiers() & Qt::ShiftModifier && m_active) {
                setFocus(hit.at); // extend from the anchor
            } else {
                const bool had = hasSelection();
                m_anchor = m_focus = hit.at;
                m_active = true;
                if (had)
                    bump(); // collapse what was selected
            }
        }
        event->accept();
        return true;
    }
    case QEvent::MouseMove: {
        auto *me = static_cast<QMouseEvent *>(event);
        if (!m_dragging || !(me->buttons() & Qt::LeftButton))
            return false;
        const QPointF p = me->position();
        m_lastPoint = p;
        if (!m_moved && (p - m_pressPoint).manhattanLength() < QGuiApplication::styleHints()->startDragDistance() / 2)
            return true;
        m_moved = true;
        const Hit hit = hitAt(p);
        if (hit.valid)
            setFocus(hit.at);
        if ((p.y() < 0 || p.y() > m_target->height()) && !m_scrollTimer.isActive()) {
            m_scrollClock.start();
            m_scrollTimer.start();
        }
        event->accept();
        return true;
    }
    case QEvent::MouseButtonRelease: {
        auto *me = static_cast<QMouseEvent *>(event);
        if (me->button() != Qt::LeftButton || !m_dragging)
            return false;
        m_dragging = false;
        m_scrollTimer.stop();
        if (!m_moved) {
            // A click: opens the link it was on (http(s) only, as openExternal.ts); collapses any selection.
            const Hit hit = hitAt(me->position());
            const QString link = hit.body ? hit.body->linkAt(hit.local.x(), hit.local.y()) : QString();
            if (!link.isEmpty() && link == m_pressLink
                && (link.startsWith(u"https://", Qt::CaseInsensitive) || link.startsWith(u"http://", Qt::CaseInsensitive)))
                emit linkActivated(link);
        }
        event->accept();
        return true;
    }
    case QEvent::MouseButtonDblClick: {
        auto *me = static_cast<QMouseEvent *>(event);
        if (me->button() != Qt::LeftButton)
            return false;
        const Hit hit = hitAt(me->position());
        if (hit.body) {
            const QPoint word = hit.body->wordAt(hit.at.pos);
            m_anchor = {hit.at.row, word.x()};
            m_focus = {hit.at.row, word.y()};
            m_active = true;
            bump();
        }
        m_dragging = true;
        m_moved = true; // the release that follows is not a click
        event->accept();
        return true;
    }
    case QEvent::HoverMove:
        updateCursor(static_cast<QHoverEvent *>(event)->position());
        return false;
    default:
        return false;
    }
}

} // namespace decibell
