pragma Singleton
import QtQuick

// The user's appearance settings that the type scale and row density follow (Settings → Appearance), the
// counterparts of the inline --ui-text-size-n / --ui-row-scale the renderer's uiStore writes on <html>.
QtObject {
    // Absolute body size in px; 0 = the theme's natural body size (14.5 Mona Sans themes, 13 console).
    property real textSizePx: 0
    property real rowScale: 1
}
