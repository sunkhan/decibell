#include "svgimage.h"

#include "texturecache.h"

#include <QPainter>
#include <QQuickWindow>
#include <QSGImageNode>
#include <QSvgRenderer>
#include <QtMath>

namespace decibell {

SvgImage::SvgImage(QQuickItem *parent) : QQuickItem(parent)
{
    setFlag(ItemHasContents);
}

void SvgImage::setSvg(const QString &s)
{
    if (s == m_svg)
        return;
    m_svg = s;
    update();
    emit svgChanged();
}

void SvgImage::geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry)
{
    QQuickItem::geometryChange(newGeometry, oldGeometry);
    if (newGeometry.size() != oldGeometry.size())
        update();
}

QSGNode *SvgImage::updatePaintNode(QSGNode *old, UpdatePaintNodeData *)
{
    auto *node = static_cast<QSGImageNode *>(old);
    QQuickWindow *w = window();
    const qreal dpr = w ? w->effectiveDevicePixelRatio() : 1;
    const int pw = qCeil(width() * dpr), ph = qCeil(height() * dpr);
    if (m_svg.isEmpty() || pw <= 0 || ph <= 0 || !w) {
        delete node;
        return nullptr;
    }
    const QString key = u"svg:" + QString::number(pw) + u'x' + QString::number(ph) + u':' + m_svg;
    const QString svg = m_svg;
    QSGTexture *t = cachedTexture(w, key, [svg, pw, ph] {
        QSvgRenderer renderer(svg.toUtf8());
        if (!renderer.isValid())
            return QImage();
        QImage img(pw, ph, QImage::Format_ARGB32_Premultiplied);
        img.fill(Qt::transparent);
        QPainter p(&img);
        p.setRenderHint(QPainter::Antialiasing);
        renderer.render(&p, QRectF(0, 0, pw, ph));
        return img;
    });
    if (!t) {
        delete node;
        return nullptr;
    }
    if (!node) {
        node = w->createImageNode();
        node->setOwnsTexture(false);
        node->setFiltering(QSGTexture::Linear);
    }
    node->setTexture(t);
    node->setRect(QRectF(0, 0, width(), height()));
    return node;
}

} // namespace decibell
