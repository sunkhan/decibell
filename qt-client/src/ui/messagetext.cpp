#include "messagetext.h"

#include "emoji.h"

#include <QAbstractTextDocumentLayout>
#include <QGuiApplication>
#include <QPainter>
#include <QQuickWindow>
#include <QRegularExpression>
#include <QSGImageNode>
#include <QSGNinePatchNode>
#include <QSGTextNode>
#include <QTextBlock>
#include <QTextCursor>
#include <QTextDocument>
#include <QTextLayout>
#include <QtMath>
#include <functional>
#include <unordered_map>

namespace decibell {

namespace {

const QUrl kBlank(QStringLiteral("decibell:blank"));

// MessageText.tsx jumboSizeFor: an unformatted message of only emoji draws them large.
qreal jumboSize(int count)
{
    return count == 1 ? 56 : count == 2 ? 44 : count == 3 ? 38 : 32;
}

QFont fontPx(const QString &family, qreal px)
{
    QFont f(family);
    f.setPointSizeF(px * 0.75); // QFont::setPixelSize takes an int; Qt 6's logical DPI is 96
    f.setHintingPreference(QFont::PreferNoHinting);
    return f;
}

// Render-thread texture caches (one window): emoji pictures by key@px, chip corners by colour/radius/dpr.
std::unordered_map<QString, QSGTexture *> &emojiTextures()
{
    static std::unordered_map<QString, QSGTexture *> m;
    return m;
}
std::unordered_map<QString, QSGTexture *> &chipTextures()
{
    static std::unordered_map<QString, QSGTexture *> m;
    return m;
}

QSGTexture *chipTexture(QQuickWindow *w, const QColor &c, qreal radius, qreal dpr)
{
    const QString key = c.name(QColor::HexArgb) + u'/' + QString::number(radius) + u'/' + QString::number(dpr);
    auto &m = chipTextures();
    if (auto it = m.find(key); it != m.end())
        return it->second;
    const int r = qCeil(radius * dpr);
    const int s = 2 * r + 3;
    QImage img(s, s, QImage::Format_ARGB32_Premultiplied);
    img.fill(Qt::transparent);
    {
        QPainter p(&img);
        p.setRenderHint(QPainter::Antialiasing);
        p.setPen(Qt::NoPen);
        p.setBrush(c);
        p.drawRoundedRect(QRectF(0, 0, s, s), r, r);
    }
    QSGTexture *t = w->createTextureFromImage(img, QQuickWindow::TextureCanUseAtlas);
    m.emplace(key, t);
    return t;
}

} // namespace

MessageText::MessageText(QQuickItem *parent) : QQuickItem(parent)
{
    setFlag(ItemHasContents);
}

MessageText::~MessageText() = default;

void MessageText::setText(const QString &t)
{
    if (t == m_text)
        return;
    m_text = t;
    m_dirtyDoc = true;
    relayout();
    emit changed();
}

void MessageText::setFontFamily(const QString &f) { if (m_family == f) return; m_family = f; m_dirtyDoc = true; relayout(); emit changed(); }
void MessageText::setMonoFamily(const QString &f) { if (m_mono == f) return; m_mono = f; m_dirtyDoc = true; relayout(); emit changed(); }
void MessageText::setPixelSize(qreal px) { if (qFuzzyCompare(m_px, px)) return; m_px = px; m_dirtyDoc = true; relayout(); emit changed(); }
void MessageText::setLineHeight(qreal px) { if (qFuzzyCompare(m_lineHeight, px)) return; m_lineHeight = px; m_dirtyDoc = true; relayout(); emit changed(); }
void MessageText::setColor(const QColor &c) { if (m_color == c) return; m_color = c; update(); emit changed(); }
void MessageText::setLinkColor(const QColor &c) { if (m_linkColor == c) return; m_linkColor = c; m_dirtyDoc = true; relayout(); emit changed(); }
void MessageText::setCodeBackground(const QColor &c) { if (m_codeBg == c) return; m_codeBg = c; update(); emit changed(); }
void MessageText::setCodeRadius(qreal r) { if (qFuzzyCompare(m_codeRadius, r)) return; m_codeRadius = r; update(); emit changed(); }
void MessageText::setBlockRadius(qreal r) { if (qFuzzyCompare(m_blockRadius, r)) return; m_blockRadius = r; update(); emit changed(); }
void MessageText::setJumboAllowed(bool on) { if (m_jumboAllowed == on) return; m_jumboAllowed = on; m_dirtyDoc = true; relayout(); emit changed(); }
QString MessageText::emojiDir() const { return QString(); }
void MessageText::setEmojiDir(const QString &dir) { Emoji::instance().setDirectory(dir); m_dirtyDoc = true; relayout(); emit changed(); }

void MessageText::geometryChange(const QRectF &newGeometry, const QRectF &oldGeometry)
{
    QQuickItem::geometryChange(newGeometry, oldGeometry);
    if (!qFuzzyCompare(newGeometry.width(), oldGeometry.width()))
        relayout();
}

// ── parsing: the richText.ts subset ─────────────────────────────────────────────────────────────────────
void MessageText::rebuild()
{
    m_dirtyDoc = false;
    m_doc = std::make_unique<QTextDocument>();
    m_doc->setDocumentMargin(0);
    m_doc->setUndoRedoEnabled(false);
    QTextOption opt = m_doc->defaultTextOption();
    opt.setWrapMode(QTextOption::WrapAnywhere); // `break-all` + overflow-wrap: anywhere
    m_doc->setDefaultTextOption(opt);
    m_doc->setDefaultFont(fontPx(m_family, m_px));
    QImage blank(1, 1, QImage::Format_ARGB32_Premultiplied);
    blank.fill(Qt::transparent);
    m_doc->addResource(QTextDocument::ImageResource, kBlank, blank);

    const QFont base = fontPx(m_family, m_px);
    const QFont mono = fontPx(m_mono, m_px * 0.85);
    const QFontMetricsF fm(base);
    m_halfLeading = qMax<qreal>(0, (m_lineHeight - (fm.ascent() + fm.descent())) / 2);

    // Unformatted, emoji only → jumbo (MessageText.tsx).
    m_emojiSize = 24;
    {
        const QString t = m_text.trimmed();
        int count = 0;
        bool onlyEmoji = !t.isEmpty();
        for (qsizetype i = 0; i < t.size() && onlyEmoji;) {
            if (t[i].isSpace()) { ++i; continue; }
            const qsizetype n = Emoji::instance().match(t, i, nullptr);
            if (!n) { onlyEmoji = false; break; }
            ++count;
            i += n;
        }
        if (m_jumboAllowed && onlyEmoji && count > 0)
            m_emojiSize = jumboSize(count);
    }

    QTextCursor cur(m_doc.get());
    QTextBlockFormat bf;
    bf.setLineHeight(m_lineHeight, QTextBlockFormat::MinimumHeight);
    cur.setBlockFormat(bf);

    QTextCharFormat plain;
    plain.setFont(base);
    plain.setForeground(m_color);

    struct State { bool bold = false, italic = false, underline = false, strike = false; };
    auto fmtFor = [&](const State &s) {
        QTextCharFormat f = plain;
        if (s.bold) f.setFontWeight(QFont::Bold);
        if (s.italic) f.setFontItalic(true);
        if (s.underline) f.setFontUnderline(true);
        if (s.strike) f.setFontStrikeOut(true);
        return f;
    };
    auto placeholder = [&](qreal w, qreal h, const QString &name) {
        QTextImageFormat img;
        img.setName(kBlank.toString());
        img.setWidth(w);
        img.setHeight(h);
        img.setProperty(QTextFormat::UserProperty, name);
        cur.insertImage(img);
    };
    const qreal emojiAscent = m_emojiSize - 0.2 * m_px; // the box's part above the baseline (align -0.2em)

    // Plain run: text with twemoji pulled out as placeholders.
    auto emitPlain = [&](QStringView s, const State &st) {
        const QTextCharFormat f = fmtFor(st);
        qsizetype last = 0;
        for (qsizetype i = 0; i < s.size();) {
            QString key;
            const qsizetype n = Emoji::instance().match(s, i, &key);
            if (n) {
                if (i > last) cur.insertText(s.mid(last, i - last).toString().replace(u'\n', QChar::LineSeparator), f);
                placeholder(m_emojiSize, emojiAscent, u"emoji:" + key);
                i += n;
                last = i;
            } else {
                ++i;
            }
        }
        if (last < s.size()) cur.insertText(s.mid(last).toString().replace(u'\n', QChar::LineSeparator), f);
    };

    // Inline markup, recursive on emphasis spans.
    std::function<void(QStringView, State)> emitInline = [&](QStringView s, State st) {
        static const QRegularExpression link(QStringLiteral("https?://\\S+"));
        qsizetype i = 0, runStart = 0;
        auto flush = [&](qsizetype end) { if (end > runStart) emitPlain(s.mid(runStart, end - runStart), st); };
        while (i < s.size()) {
            const QChar c = s[i];
            auto tryPair = [&](QStringView delim, auto apply) -> bool {
                if (!s.mid(i).startsWith(delim)) return false;
                const qsizetype close = s.indexOf(delim, i + delim.size());
                if (close <= i + delim.size()) return false;
                flush(i);
                State inner = st;
                apply(inner);
                emitInline(s.mid(i + delim.size(), close - i - delim.size()), inner);
                i = close + delim.size();
                runStart = i;
                return true;
            };
            if (c == u'`') {
                const qsizetype close = s.indexOf(u'`', i + 1);
                if (close > i + 1) {
                    flush(i);
                    QTextCharFormat cf = fmtFor(st);
                    cf.setFont(mono);
                    const int start = cur.position();
                    placeholder(6, 0, QStringLiteral("pad"));
                    cur.insertText(s.mid(i + 1, close - i - 1).toString(), cf);
                    placeholder(6, 0, QStringLiteral("pad"));
                    QTextCharFormat mark;
                    mark.setProperty(QTextFormat::UserProperty + 1, true);
                    QTextCursor sel(m_doc.get());
                    sel.setPosition(start);
                    sel.setPosition(cur.position(), QTextCursor::KeepAnchor);
                    sel.mergeCharFormat(mark);
                    i = close + 1;
                    runStart = i;
                    continue;
                }
            }
            if (tryPair(u"**", [](State &x) { x.bold = true; })) continue;
            if (tryPair(u"__", [](State &x) { x.underline = true; })) continue;
            if (tryPair(u"~~", [](State &x) { x.strike = true; })) continue;
            if (c == u'*' && tryPair(u"*", [](State &x) { x.italic = true; })) continue;
            if (c == u'h' && (i == 0 || s[i - 1].isSpace())) {
                const auto m = link.matchView(s.mid(i), 0, QRegularExpression::NormalMatch, QRegularExpression::AnchorAtOffsetMatchOption);
                if (m.hasMatch()) {
                    flush(i);
                    QTextCharFormat lf = fmtFor(st);
                    lf.setAnchor(true);
                    lf.setAnchorHref(m.captured(0));
                    lf.setForeground(m_linkColor);
                    cur.insertText(m.captured(0), lf);
                    i += m.capturedLength(0);
                    runStart = i;
                    continue;
                }
            }
            ++i;
        }
        flush(s.size());
    };

    // Fenced blocks split the message into paragraphs; the rest is inline text.
    static const QRegularExpression fence(QStringLiteral("```([A-Za-z0-9+#-]*)\\n?([\\s\\S]*?)```"));
    qsizetype at = 0;
    bool firstBlock = true;
    auto newBlock = [&](const QTextBlockFormat &f) {
        if (firstBlock) { cur.setBlockFormat(f); firstBlock = false; }
        else cur.insertBlock(f);
    };
    auto it = fence.globalMatch(m_text);
    while (it.hasNext()) {
        const auto m = it.next();
        const QStringView before = QStringView(m_text).mid(at, m.capturedStart() - at).trimmed();
        if (!before.isEmpty()) { newBlock(bf); emitInline(before, {}); }
        QTextBlockFormat code;
        code.setLineHeight(m_px * 1.45, QTextBlockFormat::MinimumHeight);
        code.setLeftMargin(12);
        code.setRightMargin(12);
        code.setTopMargin(8 + 4);
        code.setBottomMargin(8);
        code.setProperty(QTextFormat::UserProperty + 2, true);
        QTextCharFormat cf = plain;
        cf.setFont(fontPx(m_mono, m_px * 0.9));
        const QStringList lines = m.captured(2).trimmed().split(u'\n');
        // One block per code line; the box's vertical padding goes on the first and last only.
        for (qsizetype k = 0; k < lines.size(); ++k) {
            QTextBlockFormat lf = code;
            lf.setTopMargin(k == 0 ? 8 + 4 : 0);
            lf.setBottomMargin(k == lines.size() - 1 ? 8 : 0);
            newBlock(lf);
            cur.insertText(lines[k], cf);
        }
        at = m.capturedEnd();
    }
    const QStringView rest = QStringView(m_text).mid(at);
    if (!rest.trimmed().isEmpty() || firstBlock) { newBlock(bf); emitInline(at ? rest.trimmed() : rest, {}); }
}

// ── layout: chips and emoji in item coordinates ───────────────────────────────────────────────────────
void MessageText::relayout()
{
    if (m_dirtyDoc)
        rebuild();
    if (!m_doc)
        return;
    m_doc->setTextWidth(qMax<qreal>(1, width()));
    m_emoji.clear();
    m_chips.clear();
    const QFontMetricsF monoFm(fontPx(m_mono, m_px * 0.85));
    QRectF codeBlock;
    for (QTextBlock b = m_doc->begin(); b.isValid(); b = b.next()) {
        QTextLayout *lay = b.layout();
        const QPointF origin = lay->position() + QPointF(0, m_halfLeading);
        const bool isCode = b.blockFormat().property(QTextFormat::UserProperty + 2).toBool();
        if (isCode) {
            const QRectF r = m_doc->documentLayout()->blockBoundingRect(b).translated(0, m_halfLeading);
            codeBlock = codeBlock.isNull() ? QRectF(0, r.top() - 4, width(), r.height() + 4) : codeBlock.united(QRectF(0, r.top(), width(), r.height()));
            continue;
        }
        if (!codeBlock.isNull()) { m_chips.append({codeBlock, true}); codeBlock = {}; }
        // A code run is several fragments (pad, text, pad): merge contiguous ones, then one rounded box per line
        // the run covers (box-decoration-clone), so no seams.
        int codeStart = -1, codeEnd = -1;
        auto flushCode = [&] {
            if (codeStart < 0)
                return;
            for (int i = 0; i < lay->lineCount(); ++i) {
                const QTextLine line = lay->lineAt(i);
                const int ls = line.textStart(), le = ls + line.textLength();
                const int s = qMax(codeStart, ls), e = qMin(codeEnd, le);
                if (s >= e)
                    continue;
                const qreal x0 = line.cursorToX(s), x1 = line.cursorToX(e);
                const qreal baseline = origin.y() + line.y() + line.ascent();
                m_chips.append({QRectF(origin.x() + x0, baseline - monoFm.ascent() - 2, x1 - x0,
                                       monoFm.ascent() + monoFm.descent() + 4), false});
            }
            codeStart = -1;
        };
        for (QTextBlock::iterator f = b.begin(); !f.atEnd(); ++f) {
            const QTextFragment frag = f.fragment();
            const QTextCharFormat cf = frag.charFormat();
            const int rel = frag.position() - b.position();
            if (cf.property(QTextFormat::UserProperty + 1).toBool()) {
                if (codeStart < 0)
                    codeStart = rel;
                codeEnd = rel + frag.length();
                continue;
            }
            flushCode();
            if (cf.isImageFormat() && cf.property(QTextFormat::UserProperty).toString().startsWith(u"emoji:")) {
                const QTextLine line = lay->lineForTextPosition(rel);
                if (!line.isValid())
                    continue;
                const qreal x = line.cursorToX(rel);
                const qreal baseline = origin.y() + line.y() + line.ascent();
                const QString key = cf.property(QTextFormat::UserProperty).toString().mid(6);
                m_emoji.append({QRectF(origin.x() + x, baseline + 0.2 * m_px - m_emojiSize, m_emojiSize, m_emojiSize), key});
            }
        }
        flushCode();
    }
    if (!codeBlock.isNull())
        m_chips.append({codeBlock, true});
    setImplicitHeight(std::ceil(m_doc->size().height() + 0.01));
    update();
}

QSGNode *MessageText::updatePaintNode(QSGNode *old, UpdatePaintNodeData *)
{
    delete old; // rebuilt only when the content or width changed (moving the item never calls this)
    if (!m_doc || !window())
        return nullptr;
    QQuickWindow *w = window();
    const qreal dpr = w->effectiveDevicePixelRatio();
    auto *root = new QSGNode;
    // Rounded backgrounds as nine image slices of one shared rounded-square texture. Not QSGNinePatchNode: it owns
    // (and deletes) its texture, so nodes can't share one; image nodes with setOwnsTexture(false) can, and they
    // batch through the atlas.
    for (const Chip &c : m_chips) {
        const qreal r = qMin(c.block ? m_blockRadius : m_codeRadius, qMin(c.rect.width(), c.rect.height()) / 2);
        QSGTexture *t = chipTexture(w, m_codeBg, r, dpr);
        const QSize ts = t->textureSize(); // device px: corner = ceil(r*dpr), then a 3 px middle
        const qreal cs = std::ceil(r * dpr);
        const qreal xs[4] = {c.rect.left(), c.rect.left() + r, c.rect.right() - r, c.rect.right()};
        const qreal ys[4] = {c.rect.top(), c.rect.top() + r, c.rect.bottom() - r, c.rect.bottom()};
        const qreal us[4] = {0, cs, ts.width() - cs, qreal(ts.width())};
        const qreal vs[4] = {0, cs, ts.height() - cs, qreal(ts.height())};
        for (int row = 0; row < 3; ++row) {
            for (int col = 0; col < 3; ++col) {
                const QRectF dst(xs[col], ys[row], xs[col + 1] - xs[col], ys[row + 1] - ys[row]);
                if (dst.width() <= 0 || dst.height() <= 0)
                    continue;
                // Middle slices sample the texture's centre texel row / column only, so they stay flat.
                const qreal u0 = col == 1 ? ts.width() / 2.0 - 0.5 : us[col];
                const qreal u1 = col == 1 ? ts.width() / 2.0 + 0.5 : us[col + 1];
                const qreal v0 = row == 1 ? ts.height() / 2.0 - 0.5 : vs[row];
                const qreal v1 = row == 1 ? ts.height() / 2.0 + 0.5 : vs[row + 1];
                QSGImageNode *n = w->createImageNode();
                n->setTexture(t);
                n->setOwnsTexture(false);
                n->setSourceRect(QRectF(u0, v0, u1 - u0, v1 - v0));
                n->setRect(dst);
                n->setFiltering(QSGTexture::Linear);
                root->appendChildNode(n);
            }
        }
    }
    QSGTextNode *text = w->createTextNode();
    text->setColor(m_color);
    text->setLinkColor(m_linkColor);
    text->addTextDocument(QPointF(0, m_halfLeading), m_doc.get());
    root->appendChildNode(text);
    for (const Placed &e : m_emoji) {
        const int px = qCeil(e.rect.width() * dpr);
        const QString key = e.key + u'@' + QString::number(px);
        auto &cache = emojiTextures();
        QSGTexture *t = nullptr;
        if (auto it = cache.find(key); it != cache.end()) {
            t = it->second;
        } else {
            const QImage img = Emoji::instance().image(e.key, px);
            if (img.isNull()) continue;
            t = w->createTextureFromImage(img, QQuickWindow::TextureCanUseAtlas);
            cache.emplace(key, t);
        }
        QSGImageNode *n = w->createImageNode();
        n->setTexture(t);
        n->setRect(e.rect);
        n->setFiltering(QSGTexture::Linear);
        n->setOwnsTexture(false);
        root->appendChildNode(n);
    }
    return root;
}

} // namespace decibell
