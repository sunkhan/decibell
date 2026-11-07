// Textures shared by many items (emoji, code-chip corners, icons, avatar backgrounds, status dots), one cache per
// QRhi: the textures belong to it. A process-wide cache crashed the second grab of a hidden window (each grab renders
// through a new QRhi, and the cache handed out the destroyed one's textures); a lost device re-creates the scene graph
// the same way. An entry set is forgotten when its QRhi is destroyed, before the address can be reused. Its
// QSGTexture wrappers are left alone then, because their atlas may already be gone: a small leak on a rare event.
#pragma once

#include <QImage>
#include <QString>

#include <functional>

class QQuickWindow;
class QSGTexture;

namespace decibell {

// Render thread. The texture for `key` in `window`'s current graphics device, made from `make()` (in the atlas) the
// first time; null if `make` returns a null image.
QSGTexture *cachedTexture(QQuickWindow *window, const QString &key, const std::function<QImage()> &make);

} // namespace decibell
