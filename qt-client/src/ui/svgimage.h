// A small SVG (an icon, a status dot, an avatar's gradient) rasterised once at the device-pixel size it is drawn at
// and shared through the per-QRhi texture cache (texturecache.h), as Chromium rasterises inline SVG. Replaces
// QtQuick.Shapes on the curve renderer, which drew nothing again once the scene graph had been re-created (a lost
// device; every grab of a hidden window): seen in Qt 6.12, even on freshly created Shape items.
#pragma once

#include <QQuickItem>
#include <QtQml/qqmlregistration.h>

namespace decibell {

class SvgImage : public QQuickItem
{
    Q_OBJECT
    QML_ELEMENT
    // A complete SVG document; its viewBox is scaled to the item.
    Q_PROPERTY(QString svg READ svg WRITE setSvg NOTIFY svgChanged)

public:
    explicit SvgImage(QQuickItem *parent = nullptr);
    QString svg() const { return m_svg; }
    void setSvg(const QString &s);

signals:
    void svgChanged();

protected:
    QSGNode *updatePaintNode(QSGNode *old, UpdatePaintNodeData *) override;
    void geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry) override;

private:
    QString m_svg;
};

} // namespace decibell
