// A message body as the renderer draws it (features/chat/MessageText.tsx + richText.ts): the markdown subset laid
// out once with QTextDocument, drawn as scene-graph nodes: one QSGTextNode for the text, rounded chip backgrounds
// for inline code and fenced blocks (nine-patch nodes from one small texture per colour), and twemoji as atlas
// textures placed where Chromium puts the <span> (inline-block, size × size, vertical-align -0.2em).
#pragma once

#include <QColor>
#include <QFont>
#include <QQuickItem>
#include <QtQml/qqmlregistration.h>
#include <memory>

class QTextDocument;

namespace decibell {

class MessageText : public QQuickItem
{
    Q_OBJECT
    QML_ELEMENT
    Q_PROPERTY(QString text READ text WRITE setText NOTIFY changed)
    Q_PROPERTY(QString fontFamily MEMBER m_family WRITE setFontFamily NOTIFY changed)
    Q_PROPERTY(QString monoFamily MEMBER m_mono WRITE setMonoFamily NOTIFY changed)
    // CSS px, fractional (QML's font.pixelSize is an int).
    Q_PROPERTY(qreal pixelSize MEMBER m_px WRITE setPixelSize NOTIFY changed)
    Q_PROPERTY(qreal lineHeight MEMBER m_lineHeight WRITE setLineHeight NOTIFY changed)
    Q_PROPERTY(QColor color MEMBER m_color WRITE setColor NOTIFY changed)
    Q_PROPERTY(QColor linkColor MEMBER m_linkColor WRITE setLinkColor NOTIFY changed)
    Q_PROPERTY(QColor codeBackground MEMBER m_codeBg WRITE setCodeBackground NOTIFY changed)
    Q_PROPERTY(qreal codeRadius MEMBER m_codeRadius WRITE setCodeRadius NOTIFY changed)
    Q_PROPERTY(qreal blockRadius MEMBER m_blockRadius WRITE setBlockRadius NOTIFY changed)
    Q_PROPERTY(QString emojiDir READ emojiDir WRITE setEmojiDir NOTIFY changed)
    Q_PROPERTY(bool jumbo MEMBER m_jumboAllowed WRITE setJumboAllowed NOTIFY changed)

public:
    explicit MessageText(QQuickItem *parent = nullptr);
    ~MessageText() override;

    QString text() const { return m_text; }
    void setText(const QString &t);
    void setFontFamily(const QString &f);
    void setMonoFamily(const QString &f);
    void setPixelSize(qreal px);
    void setLineHeight(qreal px);
    void setColor(const QColor &c);
    void setLinkColor(const QColor &c);
    void setCodeBackground(const QColor &c);
    void setCodeRadius(qreal r);
    void setBlockRadius(qreal r);
    QString emojiDir() const;
    void setEmojiDir(const QString &dir);
    void setJumboAllowed(bool on);

signals:
    void changed();

protected:
    QSGNode *updatePaintNode(QSGNode *old, UpdatePaintNodeData *) override;
    void geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry) override;

private:
    struct Placed { QRectF rect; QString key; };   // an emoji picture, in item coordinates
    struct Chip { QRectF rect; bool block; };       // a code background
    void rebuild();   // parse + build the document (text / fonts / colours changed)
    void relayout();  // lay out at the current width, recompute chips and emoji, implicit height

    QString m_text;
    QString m_family = QStringLiteral("Mona Sans");
    QString m_mono = QStringLiteral("JetBrains Mono");
    qreal m_px = 14.5;
    qreal m_lineHeight = 21.025;
    QColor m_color = Qt::white;
    QColor m_linkColor = QColor(0x8f, 0xb4, 0xff);
    QColor m_codeBg = QColor(0x0f, 0x0d, 0x13);
    qreal m_codeRadius = 6;
    qreal m_blockRadius = 10;
    bool m_jumboAllowed = true;

    std::unique_ptr<QTextDocument> m_doc;
    qreal m_emojiSize = 24;
    qreal m_halfLeading = 0;
    QList<Placed> m_emoji;
    QList<Chip> m_chips;
    bool m_dirtyDoc = true;
};

} // namespace decibell
